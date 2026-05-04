use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestClass {
    FlowControlledPublish,
    Publish,
    Control,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestReadiness {
    Ready,
    Blocked,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScheduledRequest {
    pub class: RequestClass,
    pub readiness: RequestReadiness,
}

#[derive(Debug)]
pub struct OutboundScheduler<T> {
    queue: VecDeque<T>,
}

impl<T> Default for OutboundScheduler<T> {
    fn default() -> Self {
        Self {
            queue: VecDeque::new(),
        }
    }
}

impl<T> OutboundScheduler<T> {
    pub fn push_back(&mut self, item: T) {
        self.queue.push_back(item);
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn clear(&mut self) {
        self.queue.clear();
    }

    pub fn drain(&mut self) -> impl Iterator<Item = T> + '_ {
        self.queue.drain(..)
    }

    pub fn has_ready<F>(&self, classify: F) -> bool
    where
        F: Fn(&T) -> ScheduledRequest,
    {
        self.next_ready_index(classify).is_some()
    }

    pub fn pop_next<F>(&mut self, classify: F) -> Option<T>
    where
        F: Fn(&T) -> ScheduledRequest,
    {
        let index = self.next_ready_index(classify)?;
        self.queue.remove(index)
    }

    fn next_ready_index<F>(&self, classify: F) -> Option<usize>
    where
        F: Fn(&T) -> ScheduledRequest,
    {
        let mut can_bypass = true;

        for index in 0..self.queue.len() {
            let request = classify(&self.queue[index]);
            match (request.class, request.readiness) {
                (_, RequestReadiness::Ready) if index == 0 => {
                    return Some(index);
                }
                (RequestClass::Control, RequestReadiness::Ready) if can_bypass => {
                    return Some(index);
                }
                (RequestClass::FlowControlledPublish, RequestReadiness::Blocked) => {}
                _ => {
                    can_bypass = false;
                }
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy)]
    enum Item {
        BlockedFlowPublish,
        ReadyFlowPublish,
        ReadyPublish,
        ReadyControl,
    }

    fn classify(item: &Item) -> ScheduledRequest {
        match item {
            Item::BlockedFlowPublish => ScheduledRequest {
                class: RequestClass::FlowControlledPublish,
                readiness: RequestReadiness::Blocked,
            },
            Item::ReadyFlowPublish => ScheduledRequest {
                class: RequestClass::FlowControlledPublish,
                readiness: RequestReadiness::Ready,
            },
            Item::ReadyPublish => ScheduledRequest {
                class: RequestClass::Publish,
                readiness: RequestReadiness::Ready,
            },
            Item::ReadyControl => ScheduledRequest {
                class: RequestClass::Control,
                readiness: RequestReadiness::Ready,
            },
        }
    }

    #[test]
    fn ready_front_request_wins() {
        let mut scheduler = OutboundScheduler::default();
        scheduler.push_back(Item::ReadyFlowPublish);
        scheduler.push_back(Item::ReadyControl);

        assert!(matches!(
            scheduler.pop_next(classify),
            Some(Item::ReadyFlowPublish)
        ));
    }

    #[test]
    fn control_can_pass_blocked_flow_controlled_publish() {
        let mut scheduler = OutboundScheduler::default();
        scheduler.push_back(Item::BlockedFlowPublish);
        scheduler.push_back(Item::ReadyControl);

        assert!(matches!(
            scheduler.pop_next(classify),
            Some(Item::ReadyControl)
        ));
    }

    #[test]
    fn publish_does_not_pass_blocked_flow_controlled_publish() {
        let mut scheduler = OutboundScheduler::default();
        scheduler.push_back(Item::BlockedFlowPublish);
        scheduler.push_back(Item::ReadyPublish);

        assert!(scheduler.pop_next(classify).is_none());
    }

    #[test]
    fn control_does_not_pass_ready_publish() {
        let mut scheduler = OutboundScheduler::default();
        scheduler.push_back(Item::ReadyPublish);
        scheduler.push_back(Item::ReadyControl);

        assert!(matches!(
            scheduler.pop_next(classify),
            Some(Item::ReadyPublish)
        ));
    }

    #[test]
    fn has_ready_matches_pop_next_readiness() {
        let mut scheduler = OutboundScheduler::default();
        scheduler.push_back(Item::BlockedFlowPublish);

        assert!(!scheduler.has_ready(classify));

        scheduler.push_back(Item::ReadyControl);

        assert!(scheduler.has_ready(classify));
        assert!(matches!(
            scheduler.pop_next(classify),
            Some(Item::ReadyControl)
        ));
    }
}
