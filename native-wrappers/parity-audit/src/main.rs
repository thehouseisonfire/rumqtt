use std::path::PathBuf;

fn main() {
    if let Err(error) = run() {
        eprintln!("parity audit failed: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let command = args
        .next()
        .ok_or("usage: rumqttc-parity-audit <check|inventory|render> [--root PATH]")?;
    let mut root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    while let Some(argument) = args.next() {
        if argument != "--root" {
            return Err(format!("unknown argument {argument}"));
        }
        root = PathBuf::from(args.next().ok_or("--root needs a path")?);
    }
    let ledger = rumqttc_parity_audit::read_ledger(&root)?;
    match command.as_str() {
        "check" => {
            let inventory = rumqttc_parity_audit::inventory(&root, &ledger)?;
            rumqttc_parity_audit::validate(&root, &ledger, &inventory)?;
            rumqttc_parity_audit::check_markdown(&root, &ledger)?;
            for surface in &ledger.surfaces {
                let count = inventory
                    .iter()
                    .filter(|d| d.surface == surface.name)
                    .count();
                println!("{}: {count} declarations checked", surface.name);
            }
            println!(
                "{} reviewed boundary decisions checked",
                ledger.decisions.len()
            );
        }
        "inventory" => {
            let inventory = rumqttc_parity_audit::discover_sources(&root, &ledger)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&inventory).map_err(|e| e.to_string())?
            );
        }
        "render" => print!("{}", rumqttc_parity_audit::render(&ledger)),
        _ => return Err(format!("unknown command {command}")),
    }
    Ok(())
}
