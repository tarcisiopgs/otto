//! Which key asks for which action. The map is described in `DESIGN.md`.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::tui::app::{Action, App};

/// The action a key asks for on the screen the app is showing.
pub fn action(key: KeyEvent, app: &App) -> Option<Action> {
    // A terminal that reports releases would otherwise act twice per key.
    if key.kind != KeyEventKind::Press {
        return None;
    }
    if app.confirm.is_some() {
        return match key.code {
            KeyCode::Char('y') | KeyCode::Enter => Some(Action::Yes),
            KeyCode::Char('n') | KeyCode::Esc => Some(Action::No),
            _ => None,
        };
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return (key.code == KeyCode::Char('c')).then_some(Action::Quit);
    }
    Some(match key.code {
        KeyCode::Up | KeyCode::Char('k') => Action::Up,
        KeyCode::Down | KeyCode::Char('j') => Action::Down,
        KeyCode::PageUp => Action::PageUp,
        KeyCode::PageDown => Action::PageDown,
        KeyCode::Home | KeyCode::Char('g') => Action::Top,
        KeyCode::End | KeyCode::Char('G') => Action::Bottom,
        KeyCode::Enter => Action::Open,
        KeyCode::Esc => Action::Back,
        KeyCode::Char('q') => Action::Quit,
        KeyCode::Char('?') => Action::Help,
        KeyCode::Char('r') => Action::RunNow,
        KeyCode::Char('x') => Action::Stop,
        KeyCode::Char('p') => Action::Pause,
        KeyCode::Char('s') => Action::Skip,
        KeyCode::Char('u') => Action::Resume,
        KeyCode::Char('e') => Action::EditPrompt,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::KeyEventState;

    use super::*;
    use crate::store::{Outcome, Run, State, Trigger};
    use crate::tui::world::{JobView, Snapshot};

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn running() -> App {
        let job = JobView {
            name: "report".to_owned(),
            job: crate::config::Job {
                agent: crate::agent::Agent::Claude,
                prompt: "/p.md".into(),
                workdir: "/w".into(),
                schedule: crate::config::Schedule {
                    at: "16:05".to_owned(),
                    days: crate::config::Weekday::every_day(),
                },
                args: Vec::new(),
            },
            state: State::default(),
            runs: vec![Run {
                id: "r1".to_owned(),
                started: "2026-10-06T12:00:00Z".parse().unwrap(),
                finished: None,
                trigger: Trigger::Manual,
                outcome: Outcome::Running,
                exit_code: None,
                pid: Some(77),
            }],
            next: None,
        };
        App::new(Snapshot {
            jobs: vec![job],
            ..Snapshot::default()
        })
    }

    #[test]
    fn every_key_of_the_map_asks_for_its_action() {
        let app = running();
        for (code, expected) in [
            (KeyCode::Up, Action::Up),
            (KeyCode::Char('k'), Action::Up),
            (KeyCode::Down, Action::Down),
            (KeyCode::Char('j'), Action::Down),
            (KeyCode::PageUp, Action::PageUp),
            (KeyCode::PageDown, Action::PageDown),
            (KeyCode::Char('g'), Action::Top),
            (KeyCode::Home, Action::Top),
            (KeyCode::Char('G'), Action::Bottom),
            (KeyCode::End, Action::Bottom),
            (KeyCode::Enter, Action::Open),
            (KeyCode::Esc, Action::Back),
            (KeyCode::Char('q'), Action::Quit),
            (KeyCode::Char('?'), Action::Help),
            (KeyCode::Char('r'), Action::RunNow),
            (KeyCode::Char('x'), Action::Stop),
            (KeyCode::Char('p'), Action::Pause),
            (KeyCode::Char('s'), Action::Skip),
            (KeyCode::Char('u'), Action::Resume),
            (KeyCode::Char('e'), Action::EditPrompt),
        ] {
            assert_eq!(action(press(code), &app), Some(expected), "{code:?}");
        }
        assert_eq!(action(press(KeyCode::Char('z')), &app), None);
    }

    #[test]
    fn control_c_quits() {
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(action(key, &running()), Some(Action::Quit));
    }

    #[test]
    fn a_confirmation_takes_only_yes_and_no() {
        let mut app = running();
        app.act(Action::Stop);
        assert!(app.confirm.is_some());
        assert_eq!(action(press(KeyCode::Char('y')), &app), Some(Action::Yes));
        assert_eq!(action(press(KeyCode::Enter), &app), Some(Action::Yes));
        assert_eq!(action(press(KeyCode::Char('n')), &app), Some(Action::No));
        assert_eq!(action(press(KeyCode::Esc), &app), Some(Action::No));
        assert_eq!(action(press(KeyCode::Char('q')), &app), None);
        assert_eq!(action(press(KeyCode::Char('r')), &app), None);
    }

    #[test]
    fn a_key_being_released_asks_for_nothing() {
        // Windows reports the release as well as the press.
        let release = KeyEvent::new_with_kind_and_state(
            KeyCode::Char('r'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
            KeyEventState::NONE,
        );
        assert_eq!(action(release, &running()), None);
    }
}
