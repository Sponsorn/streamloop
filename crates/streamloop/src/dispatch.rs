//! The mode rule: the first argument names the mode; with none, START.bat's variable makes it the control
//! process, and anything else opens the app.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Control,
    Ui,
    Detach,
    Supervisor,
    Launch,
    Check,
    Protect,
    Version,
}

impl Mode {
    /// Whether this mode attaches to the console of the process that started it (`console::attach_parent`).
    /// `launch` and `supervisor` never do, so the supervisor cannot be on START.bat's console and closing
    /// that window cannot end it.
    pub fn attaches_to_console(self) -> bool {
        matches!(self, Mode::Control | Mode::Check | Mode::Protect | Mode::Version)
    }
}

/// The mode and the arguments after its word. With no mode word: Control when `start_set`, else Ui,
/// and every argument passes through to it.
pub fn pick(args: &[String], start_set: bool) -> (Mode, Vec<String>) {
    let named = match args.first().map(String::as_str) {
        Some("control") => Mode::Control,
        Some("ui") => Mode::Ui,
        Some("detach") => Mode::Detach,
        Some("supervisor") => Mode::Supervisor,
        Some("launch") => Mode::Launch,
        Some("check") => Mode::Check,
        Some("protect") => Mode::Protect,
        Some("--version") => Mode::Version,
        _ if start_set => return (Mode::Control, args.to_vec()),
        _ => return (Mode::Ui, args.to_vec()),
    };
    (named, args[1..].to_vec())
}

/// Every START.bat since 3.1.0 sets `STREAMLOOP_START` (`first`, `restart` or `rollback`) for the exe it runs.
pub fn start_set() -> bool {
    std::env::var_os("STREAMLOOP_START").is_some_and(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn no_word_under_start_bat_is_the_control_process() {
        // Arrange
        let none = args(&[]);

        // Act
        let picked = pick(&none, true);

        // Assert
        assert_eq!(picked, (Mode::Control, args(&[])));
    }

    #[test]
    fn no_word_otherwise_is_the_app() {
        // Arrange
        let none = args(&[]);

        // Act
        let picked = pick(&none, false);

        // Assert
        assert_eq!(picked, (Mode::Ui, args(&[])));
    }

    #[test]
    fn flags_pass_through_without_a_word() {
        // Arrange
        let flags = args(&["--root", "D"]);

        // Act
        let under_start_bat = pick(&flags, true);
        let otherwise = pick(&flags, false);

        // Assert
        assert_eq!(under_start_bat, (Mode::Control, args(&["--root", "D"])));
        assert_eq!(otherwise, (Mode::Ui, args(&["--root", "D"])));
    }

    #[test]
    fn a_named_mode_wins_over_the_variable() {
        // Arrange
        let ui = args(&["ui", "--tray"]);
        let control = args(&["control", "--root", "D"]);

        // Act
        let ui_under_start_bat = pick(&ui, true);
        let control_otherwise = pick(&control, false);

        // Assert
        assert_eq!(ui_under_start_bat, (Mode::Ui, args(&["--tray"])));
        assert_eq!(control_otherwise, (Mode::Control, args(&["--root", "D"])));
    }

    #[test]
    fn every_mode_word_names_its_mode_and_is_dropped() {
        for (word, mode) in [
            ("control", Mode::Control),
            ("ui", Mode::Ui),
            ("detach", Mode::Detach),
            ("supervisor", Mode::Supervisor),
            ("launch", Mode::Launch),
            ("check", Mode::Check),
            ("protect", Mode::Protect),
            ("--version", Mode::Version),
        ] {
            for start_set in [true, false] {
                // Act
                let picked = pick(&args(&[word, "--root", "D"]), start_set);

                // Assert
                assert_eq!(picked, (mode, args(&["--root", "D"])), "{word} with the variable {start_set}");
            }
        }
    }

    #[test]
    fn an_unknown_word_passes_through_as_an_argument() {
        // Act
        let under_start_bat = pick(&args(&["bogus"]), true);
        let otherwise = pick(&args(&["bogus"]), false);

        // Assert
        assert_eq!(under_start_bat, (Mode::Control, args(&["bogus"])));
        assert_eq!(otherwise, (Mode::Ui, args(&["bogus"])));
    }

    #[test]
    fn only_the_console_modes_attach_to_a_console() {
        for (mode, attaches) in [
            (Mode::Control, true),
            (Mode::Check, true),
            (Mode::Protect, true),
            (Mode::Version, true),
            (Mode::Ui, false),
            (Mode::Detach, false),
            (Mode::Launch, false),
            (Mode::Supervisor, false),
        ] {
            // Act + Assert
            assert_eq!(mode.attaches_to_console(), attaches, "{mode:?}");
        }
    }
}
