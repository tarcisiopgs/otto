//! Which key asks for which action. The map is described in `DESIGN.md`.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::tui::app::{Action, App, Screen};
use crate::tui::form::{Edit, Focus};

/// The action a key asks for on the screen the app is showing.
pub fn action(key: KeyEvent, app: &App) -> Option<Action> {
    // A terminal that reports releases would otherwise act twice per key.
    if key.kind != KeyEventKind::Press {
        return None;
    }
    // A key held with one of these is a shortcut of the terminal or of the
    // window manager, not the plain key.
    let other = KeyModifiers::SUPER | KeyModifiers::HYPER | KeyModifiers::META;
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    // Control and alt together are AltGr, as Windows reports it: the key that
    // types `@ \ { [ ~` on many keyboards. The character is what was typed.
    let altgr = alt && control && matches!(key.code, KeyCode::Char(_));
    if key.modifiers.intersects(other) || (alt && !altgr) {
        return None;
    }
    let control = control && !altgr;
    if app.confirm.is_some() {
        // Only `y` says yes: Enter is too easy to press for a question that
        // stops a run. Ctrl-C backs out of the question, not of otto.
        return match key.code {
            KeyCode::Char('c') if control => Some(Action::No),
            _ if control => None,
            KeyCode::Char('y') => Some(Action::Yes),
            KeyCode::Char('n') | KeyCode::Esc => Some(Action::No),
            _ => None,
        };
    }
    if app.screen == Screen::Form {
        return on_the_form(key, control, app);
    }
    if control {
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
        KeyCode::Char('n') => Action::New,
        KeyCode::Char('E') => Action::EditJob,
        KeyCode::Char('d') => Action::Delete,
        KeyCode::Char('S') => Action::Sync,
        KeyCode::Char('a') => Action::Apply,
        _ => return None,
    })
}

/// The keys of the form. While a field takes text every letter is text, so
/// what the other screens do with one key is done here with control or with
/// a key that types nothing.
fn on_the_form(key: KeyEvent, control: bool, app: &App) -> Option<Action> {
    let focus = app.form.as_ref().map(|form| form.focus);
    if control {
        return match key.code {
            KeyCode::Char('s') => Some(Action::Save),
            KeyCode::Char('c') => Some(Action::Back),
            _ => None,
        };
    }
    Some(match key.code {
        KeyCode::Esc => Action::Back,
        KeyCode::Tab | KeyCode::Down => Action::NextField,
        KeyCode::BackTab | KeyCode::Up => Action::PrevField,
        // The arguments are one to a line; anywhere else Enter moves on.
        KeyCode::Enter if focus == Some(Focus::Args) => Action::Input(Edit::Insert('\n')),
        KeyCode::Enter => Action::NextField,
        KeyCode::Char(' ') if matches!(focus, Some(Focus::Agent | Focus::Days)) => Action::Toggle,
        KeyCode::Char(c) => Action::Input(Edit::Insert(c)),
        KeyCode::Backspace => Action::Input(Edit::Backspace),
        KeyCode::Delete => Action::Input(Edit::Delete),
        KeyCode::Left => Action::Input(Edit::Left),
        KeyCode::Right => Action::Input(Edit::Right),
        KeyCode::Home => Action::Input(Edit::Home),
        KeyCode::End => Action::Input(Edit::End),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::KeyEventState;

    use super::*;
    use crate::store::{Outcome, Run, State, Trigger};
    use crate::tui::form::{Edit, Focus};
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
                notify: crate::notify::Level::default(),
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
    fn a_key_held_with_alt_is_another_key() {
        let key = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::ALT);
        assert_eq!(action(key, &running()), None);
        let key = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::SUPER);
        assert_eq!(action(key, &running()), None);
        // Shift is how `G` is typed.
        let key = KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT);
        assert_eq!(action(key, &running()), Some(Action::Bottom));
    }

    #[test]
    fn the_job_list_has_keys_to_create_edit_and_delete() {
        let app = running();
        assert_eq!(action(press(KeyCode::Char('n')), &app), Some(Action::New));
        assert_eq!(
            action(press(KeyCode::Char('E')), &app),
            Some(Action::EditJob)
        );
        assert_eq!(
            action(press(KeyCode::Char('d')), &app),
            Some(Action::Delete)
        );
    }

    fn on_the_form(focus: Focus) -> App {
        let mut app = running();
        app.open_form(Ok(String::new()), None);
        app.form.as_mut().unwrap().focus = focus;
        app
    }

    #[test]
    fn on_the_form_the_letters_are_text() {
        let app = on_the_form(Focus::Workdir);
        for c in ['q', 'r', 'x', 'n', 'E', 'd', '?', 'j', 'k', 'g', ' ', 'ã'] {
            assert_eq!(
                action(press(KeyCode::Char(c)), &app),
                Some(Action::Input(Edit::Insert(c))),
                "{c:?}"
            );
        }
        // A capital comes with shift held.
        let shifted = KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT);
        assert_eq!(
            action(shifted, &app),
            Some(Action::Input(Edit::Insert('A')))
        );
    }

    #[test]
    fn the_form_has_its_own_keys() {
        let app = on_the_form(Focus::Workdir);
        for (code, expected) in [
            (KeyCode::Tab, Action::NextField),
            (KeyCode::Down, Action::NextField),
            (KeyCode::Enter, Action::NextField),
            (KeyCode::BackTab, Action::PrevField),
            (KeyCode::Up, Action::PrevField),
            (KeyCode::Esc, Action::Back),
            (KeyCode::Backspace, Action::Input(Edit::Backspace)),
            (KeyCode::Delete, Action::Input(Edit::Delete)),
            (KeyCode::Left, Action::Input(Edit::Left)),
            (KeyCode::Right, Action::Input(Edit::Right)),
            (KeyCode::Home, Action::Input(Edit::Home)),
            (KeyCode::End, Action::Input(Edit::End)),
        ] {
            assert_eq!(action(press(code), &app), Some(expected), "{code:?}");
        }
        let save = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert_eq!(action(save, &app), Some(Action::Save));
        let leave = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(action(leave, &app), Some(Action::Back));
        // Shift-tab arrives as a back-tab with shift held.
        let back = KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT);
        assert_eq!(action(back, &app), Some(Action::PrevField));
    }

    /// AltGr, which some keyboards need for `@ \ { [ ~`, arrives on Windows
    /// as control and alt held together.
    #[test]
    fn a_character_typed_with_altgr_is_that_character() {
        let altgr = KeyModifiers::CONTROL | KeyModifiers::ALT;
        let app = on_the_form(Focus::Workdir);
        for c in ['@', '\\', '{', '[', '~'] {
            let key = KeyEvent::new(KeyCode::Char(c), altgr);
            assert_eq!(
                action(key, &app),
                Some(Action::Input(Edit::Insert(c))),
                "{c:?}"
            );
        }
        // Alt alone is still a shortcut of the terminal, on the form too.
        let alt = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT);
        assert_eq!(action(alt, &app), None);
        // And control alone does not type.
        let control = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL);
        assert_eq!(action(control, &app), None);
    }

    #[test]
    fn space_marks_a_choice_and_enter_breaks_a_line_of_arguments() {
        for focus in [Focus::Agent, Focus::Days] {
            let app = on_the_form(focus);
            assert_eq!(
                action(press(KeyCode::Char(' ')), &app),
                Some(Action::Toggle)
            );
        }
        let args = on_the_form(Focus::Args);
        assert_eq!(
            action(press(KeyCode::Enter), &args),
            Some(Action::Input(Edit::Insert('\n')))
        );
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
        // Enter is too easy to press for a question that stops a run.
        assert_eq!(action(press(KeyCode::Enter), &app), None);
        let control_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(action(control_c, &app), Some(Action::No));
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

    #[test]
    fn the_sync_has_a_key_to_review_and_one_to_apply() {
        let app = running();
        assert_eq!(action(press(KeyCode::Char('S')), &app), Some(Action::Sync));
        assert_eq!(action(press(KeyCode::Char('a')), &app), Some(Action::Apply));
        // The lowercase one is still the skip.
        assert_eq!(action(press(KeyCode::Char('s')), &app), Some(Action::Skip));
    }
}
