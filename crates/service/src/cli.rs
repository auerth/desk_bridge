//! Command line of `deskbridge-service.exe` (portable so it can be unit-tested).

/// Options of `--console`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConsoleOptions {
    /// Never call `SendSAS` (answers `send_sas` with an error instead).
    pub no_sas: bool,
    /// Do not start or watch the agent.
    pub no_agent: bool,
    /// Do not start the secure-desktop helper.
    pub no_helper: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// No arguments: started by the SCM.
    Service,
    /// `--helper ...`: the arguments after `--helper` are parsed by `helper_proc::parse_args`.
    Helper(Vec<String>),
    /// `--console [--no-sas] [--no-agent] [--no-helper]`
    Console(ConsoleOptions),
    Version,
    Help,
}

pub const USAGE: &str = "\
deskbridge-service.exe                 run as Windows service (started by the SCM)
deskbridge-service.exe --console       run the service logic in the foreground (needs SYSTEM,
                                       e.g. `psexec -s -w <dir> deskbridge-service.exe --console`)
    --no-sas                           never call SendSAS (Ctrl+Alt+Del requests are refused)
    --no-agent                         do not start/watch the agent
    --no-helper                        do not start the secure-desktop helper
deskbridge-service.exe --helper [--user-sid S-1-5-...] [--parent-pid N] [--pipe NAME]
                                       secure-desktop helper (started by the service)
deskbridge-service.exe --version | --help";

pub fn parse(args: &[String]) -> Result<Mode, String> {
    let Some(first) = args.first() else { return Ok(Mode::Service) };
    match first.as_str() {
        "--helper" => Ok(Mode::Helper(args[1..].to_vec())),
        "--console" => {
            let mut o = ConsoleOptions::default();
            for a in &args[1..] {
                match a.as_str() {
                    "--no-sas" => o.no_sas = true,
                    "--no-agent" => o.no_agent = true,
                    "--no-helper" => o.no_helper = true,
                    other => return Err(format!("unknown option '{other}'")),
                }
            }
            Ok(Mode::Console(o))
        }
        "--version" | "-V" => Ok(Mode::Version),
        "--help" | "-h" | "/?" => Ok(Mode::Help),
        other => Err(format!("unknown argument '{other}'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_string()).collect()
    }

    #[test]
    fn modes() {
        assert_eq!(parse(&[]), Ok(Mode::Service));
        assert_eq!(parse(&s(&["--version"])), Ok(Mode::Version));
        assert_eq!(parse(&s(&["--help"])), Ok(Mode::Help));
        assert_eq!(
            parse(&s(&["--helper", "--parent-pid", "4"])),
            Ok(Mode::Helper(s(&["--parent-pid", "4"])))
        );
        assert_eq!(
            parse(&s(&["--console", "--no-sas", "--no-agent"])),
            Ok(Mode::Console(ConsoleOptions { no_sas: true, no_agent: true, no_helper: false }))
        );
        assert!(parse(&s(&["--console", "--wat"])).is_err());
        assert!(parse(&s(&["--wat"])).is_err());
    }
}
