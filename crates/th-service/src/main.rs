//! `th-service.exe`: the TunnelHop Windows service and its administration commands.

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    use std::process::ExitCode;

    use th_service::killswitch::WfpKillSwitch;
    use th_service::svc::{self, describe};
    use th_service::{logging, runtimes};

    const USAGE: &str = "usage: th-service <command>

  install     register the TunnelHop service and start it (administrator)
  uninstall   stop and remove the service and its data (administrator)
  disengage   remove the kill-switch filters (administrator)
  runtimes    show the installed WireGuard and OpenVPN versions
  run         used by the service control manager";

    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let [command] = arguments.as_slice() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    if command != "run" {
        logging::to_stderr();
    }
    // A dev build says so on every command. This also keeps the marker in the binary, so CI's
    // check that release builds lack it cannot pass by accident.
    #[cfg(feature = "dev-safety")]
    log::warn!(
        "{}: dev build, an unconfirmed kill-switch engage lifts itself",
        th_service::killswitch::dev_safety::MARKER
    );
    let result = match command.as_str() {
        "run" => svc::run().map_err(|e| {
            let hint = "`run` is for the service control manager; use `install`";
            format!("{}: {hint}", describe(&e)).into()
        }),
        "install" => svc::install(),
        "uninstall" => svc::uninstall(),
        // The recovery command: works whatever state the service is in, or without it.
        "disengage" => WfpKillSwitch::disengage_now()
            .map(|()| log::info!("kill-switch removed; traffic flows normally"))
            .map_err(Into::into),
        "runtimes" => {
            for runtime in runtimes::discover() {
                println!("{:?}: {:?}", runtime.protocol, runtime.availability);
            }
            Ok(())
        }
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("th-service {command}: {}", describe(e.as_ref()));
            ExitCode::FAILURE
        }
    }
}
#[cfg(not(windows))]
fn main() {
    eprintln!("th-service runs on Windows only");
    std::process::exit(1);
}
