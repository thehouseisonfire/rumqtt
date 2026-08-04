use std::error::Error;
use std::fmt::{self, Debug, Formatter};
use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::Arc;

#[cfg(feature = "system-srv-resolver")]
use std::sync::OnceLock;

#[cfg(feature = "system-srv-resolver")]
use hickory_resolver::TokioResolver;
#[cfg(feature = "system-srv-resolver")]
use hickory_resolver::net::NetError;
#[cfg(feature = "system-srv-resolver")]
use hickory_resolver::proto::rr::RData;
use rand::seq::SliceRandom;
use rand::{Rng, RngExt};

pub const MAX_SRV_TARGETS: usize = 32;

type BoxError = Box<dyn Error + Send + Sync>;
type SrvLookupFuture =
    Pin<Box<dyn Future<Output = Result<Vec<SrvRecord>, SrvLookupError>> + Send + 'static>>;
type SrvLookupCallback = dyn Fn(String) -> SrvLookupFuture + Send + Sync;

/// One DNS SRV resource record, independent of the resolver implementation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SrvRecord {
    /// Lower values are attempted before higher values.
    pub priority: u16,
    /// Relative selection weight among records with the same priority.
    pub weight: u16,
    /// Port advertised for the target service.
    pub port: u16,
    /// DNS hostname of the service target.
    pub target: String,
}

/// Broad classification for an SRV lookup failure.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SrvLookupErrorKind {
    /// Host-system resolver construction failed.
    Initialization,
    /// The queried owner does not exist.
    NxDomain,
    /// The owner exists but has no SRV records.
    NoRecords,
    /// The DNS query failed for another reason.
    Query,
    /// An application-provided resolver returned an error.
    Custom,
}

/// Resolver-independent SRV lookup failure which retains its underlying source.
#[derive(Debug)]
pub struct SrvLookupError {
    kind: SrvLookupErrorKind,
    source: BoxError,
}

impl SrvLookupError {
    /// Wrap an application resolver error.
    #[must_use]
    pub fn custom<E>(error: E) -> Self
    where
        E: Error + Send + Sync + 'static,
    {
        Self {
            kind: SrvLookupErrorKind::Custom,
            source: Box::new(error),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> SrvLookupErrorKind {
        self.kind
    }

    #[cfg(feature = "system-srv-resolver")]
    fn hickory_initialization(error: NetError) -> Self {
        Self {
            kind: SrvLookupErrorKind::Initialization,
            source: Box::new(error),
        }
    }

    #[cfg(feature = "system-srv-resolver")]
    fn hickory_query(error: NetError) -> Self {
        let kind = if error.is_nx_domain() {
            SrvLookupErrorKind::NxDomain
        } else if error.is_no_records_found() {
            SrvLookupErrorKind::NoRecords
        } else {
            SrvLookupErrorKind::Query
        };
        Self {
            kind,
            source: Box::new(error),
        }
    }
}

impl fmt::Display for SrvLookupError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let kind = match self.kind {
            SrvLookupErrorKind::Initialization => "resolver initialization",
            SrvLookupErrorKind::NxDomain => "NXDOMAIN",
            SrvLookupErrorKind::NoRecords => "no SRV records",
            SrvLookupErrorKind::Query => "resolver query",
            SrvLookupErrorKind::Custom => "custom resolver",
        };
        write!(formatter, "{kind} failed: {}", self.source)
    }
}

impl Error for SrvLookupError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// Effective DNS SRV resolver mode.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SrvResolverMode {
    /// An application-provided resolver overrides the built-in backend.
    Custom,
    /// The opt-in host-system Hickory backend is active.
    System,
    /// No resolver backend is available in this build or configuration.
    Unavailable,
}

/// Cloneable application-provided asynchronous DNS SRV resolver.
#[derive(Clone)]
pub struct SrvResolver {
    callback: Arc<SrvLookupCallback>,
}

impl SrvResolver {
    /// Create a resolver backed by an application callback.
    #[must_use]
    pub fn new<F, Fut>(resolver: F) -> Self
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Vec<SrvRecord>, SrvLookupError>> + Send + 'static,
    {
        Self {
            callback: Arc::new(move |owner| Box::pin(resolver(owner))),
        }
    }

    pub(crate) async fn lookup(&self, owner: String) -> Result<Vec<SrvRecord>, SrvLookupError> {
        (self.callback)(owner).await
    }
}

#[cfg(feature = "system-srv-resolver")]
#[derive(Clone)]
pub struct SystemSrvResolver {
    resolver: Arc<OnceLock<Result<TokioResolver, NetError>>>,
}

#[cfg(feature = "system-srv-resolver")]
impl SystemSrvResolver {
    pub(crate) fn new() -> Self {
        Self {
            resolver: Arc::new(OnceLock::new()),
        }
    }

    fn resolver(&self) -> Result<&TokioResolver, SrvLookupError> {
        self.resolver
            .get_or_init(|| {
                TokioResolver::builder_tokio().and_then(hickory_resolver::ResolverBuilder::build)
            })
            .as_ref()
            .map_err(|error| SrvLookupError::hickory_initialization(error.clone()))
    }

    pub(crate) async fn lookup(&self, owner: String) -> Result<Vec<SrvRecord>, SrvLookupError> {
        let lookup = self
            .resolver()?
            .srv_lookup(owner)
            .await
            .map_err(SrvLookupError::hickory_query)?;
        Ok(lookup
            .answers()
            .iter()
            .filter_map(|record| match &record.data {
                RData::SRV(record) => Some(SrvRecord {
                    priority: record.priority,
                    weight: record.weight,
                    port: record.port,
                    target: record.target.to_utf8(),
                }),
                _ => None,
            })
            .collect())
    }
}

pub enum EffectiveSrvResolver {
    Custom(SrvResolver),
    #[cfg(feature = "system-srv-resolver")]
    System(SystemSrvResolver),
    #[cfg(not(feature = "system-srv-resolver"))]
    Unavailable,
}

impl EffectiveSrvResolver {
    pub(crate) async fn lookup(
        &self,
        owner: String,
    ) -> Option<Result<Vec<SrvRecord>, SrvLookupError>> {
        match self {
            Self::Custom(resolver) => Some(resolver.lookup(owner).await),
            #[cfg(feature = "system-srv-resolver")]
            Self::System(resolver) => Some(resolver.lookup(owner).await),
            #[cfg(not(feature = "system-srv-resolver"))]
            Self::Unavailable => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedSrvTarget {
    pub(crate) priority: u16,
    pub(crate) weight: u16,
    pub(crate) port: u16,
    pub(crate) target: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SrvAnswerError {
    ServiceUnavailable,
    TooMany { count: usize },
    NoUsableTargets { rejected: usize },
}

pub fn prepare_srv_targets<R: Rng + ?Sized>(
    records: Vec<SrvRecord>,
    rng: &mut R,
) -> Result<Vec<ResolvedSrvTarget>, SrvAnswerError> {
    if records.len() == 1 && records[0].target == "." {
        return Err(SrvAnswerError::ServiceUnavailable);
    }

    let mut rejected = 0;
    let mut targets = Vec::new();
    for record in records {
        if record.target == "." {
            continue;
        }
        let Some(target) = normalize_target(&record.target) else {
            rejected += 1;
            continue;
        };
        if record.port == 0 {
            rejected += 1;
            continue;
        }
        targets.push(ResolvedSrvTarget {
            priority: record.priority,
            weight: record.weight,
            port: record.port,
            target,
        });
    }
    if targets.len() > MAX_SRV_TARGETS {
        return Err(SrvAnswerError::TooMany {
            count: targets.len(),
        });
    }
    if targets.is_empty() {
        return Err(SrvAnswerError::NoUsableTargets { rejected });
    }

    targets.sort_by_key(|target| target.priority);
    let mut ordered = Vec::with_capacity(targets.len());
    let mut start = 0;
    while start < targets.len() {
        let priority = targets[start].priority;
        let end = targets[start..]
            .iter()
            .position(|target| target.priority != priority)
            .map_or(targets.len(), |offset| start + offset);
        let mut group = targets[start..end].to_vec();
        order_priority_group(&mut group, rng, &mut ordered);
        start = end;
    }
    Ok(ordered)
}

fn order_priority_group<R: Rng + ?Sized>(
    group: &mut Vec<ResolvedSrvTarget>,
    rng: &mut R,
    ordered: &mut Vec<ResolvedSrvTarget>,
) {
    while !group.is_empty() {
        if group.iter().all(|target| target.weight == 0) {
            group.shuffle(rng);
            ordered.append(group);
            return;
        }
        group.sort_by_key(|target| target.weight != 0);
        let sum = group
            .iter()
            .map(|target| u32::from(target.weight))
            .sum::<u32>();
        let draw = rng.random_range(0..=sum);
        let mut running = 0_u32;
        let index = group
            .iter()
            .position(|target| {
                running += u32::from(target.weight);
                running >= draw
            })
            .expect("the inclusive draw cannot exceed the weight sum");
        ordered.push(group.remove(index));
    }
}

fn normalize_target(target: &str) -> Option<String> {
    let target = target.strip_suffix('.').unwrap_or(target);
    if target.is_empty() || target.parse::<IpAddr>().is_ok() {
        return None;
    }
    let labels = target.split('.').collect::<Vec<_>>();
    let total = labels.iter().map(|label| label.len()).sum::<usize>() + labels.len() - 1;
    if total > 253
        || labels.iter().any(|label| {
            label.is_empty()
                || label.len() > 63
                || !label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                || !label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                || !label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
        })
    {
        return None;
    }
    Some(target.to_ascii_lowercase())
}

impl Debug for SrvResolver {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SrvResolver")
            .field("mode", &SrvResolverMode::Custom)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;
    use std::convert::Infallible;

    struct ZeroRng;

    impl rand::TryRng for ZeroRng {
        type Error = Infallible;

        fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
            Ok(0)
        }

        fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
            Ok(0)
        }

        fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Self::Error> {
            dst.fill(0);
            Ok(())
        }
    }

    fn record(priority: u16, weight: u16, port: u16, target: &str) -> SrvRecord {
        SrvRecord {
            priority,
            weight,
            port,
            target: target.to_owned(),
        }
    }

    #[test]
    fn validates_and_normalizes_complete_answers() {
        let mut rng = StdRng::seed_from_u64(1);
        let targets = prepare_srv_targets(
            vec![
                record(0, 0, 1883, "."),
                record(0, 0, 0, "zero.example"),
                record(0, 0, 1883, "127.0.0.1"),
                record(0, 0, 1883, "Broker.EXAMPLE."),
            ],
            &mut rng,
        )
        .unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].target, "broker.example");
    }

    #[test]
    fn dot_only_and_empty_usable_answers_are_distinct() {
        let mut rng = StdRng::seed_from_u64(2);
        assert_eq!(
            prepare_srv_targets(vec![record(0, 0, 0, ".")], &mut rng),
            Err(SrvAnswerError::ServiceUnavailable)
        );
        assert_eq!(
            prepare_srv_targets(
                vec![record(0, 0, 0, "bad.example"), record(0, 0, 1883, ".")],
                &mut rng,
            ),
            Err(SrvAnswerError::NoUsableTargets { rejected: 1 })
        );
    }

    #[test]
    fn enforces_the_usable_target_bound_without_truncation() {
        let records = (0..=MAX_SRV_TARGETS)
            .map(|index| record(0, 0, 1883, &format!("broker-{index}.example")))
            .collect();
        assert_eq!(
            prepare_srv_targets(records, &mut StdRng::seed_from_u64(3)),
            Err(SrvAnswerError::TooMany {
                count: MAX_SRV_TARGETS + 1
            })
        );
    }

    #[test]
    fn exhausts_priorities_and_selects_without_replacement() {
        let records = vec![
            record(20, 1, 1883, "backup.example"),
            record(10, 1, 1883, "a.example"),
            record(10, 4, 1883, "b.example"),
        ];
        let ordered = prepare_srv_targets(records, &mut StdRng::seed_from_u64(4)).unwrap();
        assert_eq!(
            ordered
                .iter()
                .map(|target| target.priority)
                .collect::<Vec<_>>(),
            vec![10, 10, 20]
        );
        let mut names = ordered
            .iter()
            .map(|target| target.target.as_str())
            .collect::<Vec<_>>();
        names.sort_unstable();
        assert_eq!(names, vec!["a.example", "b.example", "backup.example"]);
    }

    #[test]
    fn maximum_weights_do_not_overflow() {
        let records = (0..MAX_SRV_TARGETS)
            .map(|index| record(0, u16::MAX, 1883, &format!("broker-{index}.example")))
            .collect();
        assert_eq!(
            prepare_srv_targets(records, &mut StdRng::seed_from_u64(5))
                .unwrap()
                .len(),
            MAX_SRV_TARGETS
        );
    }

    #[test]
    fn inclusive_zero_draw_can_select_a_zero_weight_record() {
        let records = vec![
            record(0, 10, 1883, "weighted.example"),
            record(0, 0, 1883, "zero.example"),
        ];
        let ordered = prepare_srv_targets(records, &mut ZeroRng).unwrap();
        assert_eq!(ordered[0].target, "zero.example");
    }

    #[test]
    fn all_zero_weights_are_seeded_and_not_response_ordered() {
        let records = vec![
            record(0, 0, 1883, "a.example"),
            record(0, 0, 1883, "b.example"),
            record(0, 0, 1883, "c.example"),
            record(0, 0, 1883, "d.example"),
        ];
        let first = prepare_srv_targets(records.clone(), &mut StdRng::seed_from_u64(6)).unwrap();
        let repeated = prepare_srv_targets(records.clone(), &mut StdRng::seed_from_u64(6)).unwrap();
        assert_eq!(first, repeated);
        assert_ne!(
            first
                .iter()
                .map(|target| &target.target)
                .collect::<Vec<_>>(),
            records
                .iter()
                .map(|record| &record.target)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn higher_weight_is_selected_first_materially_more_often() {
        let mut heavy_first = 0;
        for seed in 0..1_000 {
            let records = vec![
                record(0, 1, 1883, "light.example"),
                record(0, 20, 1883, "heavy.example"),
            ];
            let ordered = prepare_srv_targets(records, &mut StdRng::seed_from_u64(seed)).unwrap();
            heavy_first += usize::from(ordered[0].target == "heavy.example");
        }
        assert!(
            heavy_first > 800,
            "heavy target selected first {heavy_first} times"
        );
    }

    #[tokio::test]
    async fn custom_resolver_preserves_owned_records_and_errors() {
        let resolver = SrvResolver::new(|owner| async move {
            assert_eq!(owner, "_mqtt._tcp.example.");
            Ok(vec![record(0, 0, 1883, "broker.example")])
        });
        assert_eq!(
            resolver
                .lookup("_mqtt._tcp.example.".to_owned())
                .await
                .unwrap()
                .len(),
            1
        );

        let resolver = SrvResolver::new(|_| async {
            Err(SrvLookupError::custom(std::io::Error::other("scripted")))
        });
        let error = resolver.lookup("ignored.".to_owned()).await.unwrap_err();
        assert_eq!(error.kind(), SrvLookupErrorKind::Custom);
        assert!(error.source().is_some());
    }

    #[cfg(feature = "system-srv-resolver")]
    #[test]
    fn system_resolver_initialization_is_lazy_shared_and_structured() {
        let resolver = SystemSrvResolver::new();
        let clone = resolver.clone();
        assert!(Arc::ptr_eq(&resolver.resolver, &clone.resolver));
        assert!(resolver.resolver.get().is_none());

        if let Err(error) = resolver.resolver() {
            assert_eq!(error.kind(), SrvLookupErrorKind::Initialization);
            assert!(error.source().is_some());
        }
        assert!(resolver.resolver.get().is_some());
    }
}
