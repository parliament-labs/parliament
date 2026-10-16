//! psql's `\if` / `\elif` / `\else` / `\endif` — its conditional stack
//! (`fe_utils/conditional.c`) and the commands that move it
//! (`exec_command_if` … in `command.c`).
//!
//! A block spans as many Enters as it takes, as in psql: the stack lives
//! on the shell (`ShellTab.conditionals`), and the shell reads its input
//! in order (`shell::process_input`), each branching command where it
//! stands. In an inactive branch psql runs nothing: each statement is
//! "query ignored", each other command "command ignored" (`IGNORED_QUERY`,
//! `ignored_command`).
//!
//! The query buffer carries across a block as psql's does: what an active
//! branch adds stays; what an inactive one adds is dropped at the next
//! branching command, back to the buffer saved when the branch began
//! (`save_query_text_state` / `discard_query_text`).

/// Where a block stands (psql's `ifState`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum IfState {
    /// In an `\if` / `\elif` that's true, under active branches.
    True,
    /// In an `\if` / `\elif` that's false, no branch taken yet, under
    /// active branches.
    False,
    /// In an `\elif` after a taken branch, or a whole block under an
    /// inactive branch.
    Ignored,
    /// In an `\else` that's taken.
    ElseTrue,
    /// In an `\else` that isn't.
    ElseFalse,
}

/// One open block: where it stands, and the query buffer to go back to
/// when an inactive branch of it ends.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct IfFrame {
    pub state: IfState,
    pub saved: String,
}

/// A branching command, as the plane read it: `\if` / `\elif` with its
/// expression — true or false, or psql's complaint about it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Branch {
    If(Result<bool, String>),
    Elif(Result<bool, String>),
    Else,
    Endif,
}

/// What psql says for a statement in an inactive branch.
pub(crate) const IGNORED_QUERY: &str = "query ignored; use \\endif or Ctrl-C to exit current \\if block";

/// What psql says for another command there (`\echo` …), by its name.
pub(crate) fn ignored_command(name: &str) -> String {
    format!("\\{name} command ignored; use \\endif or Ctrl-C to exit current \\if block")
}

/// What psql says when Ctrl-C leaves the innermost block.
pub(crate) const ESCAPED: &str = "\\if: escaped";

/// Whether what's typed now runs: no block open, or the innermost in a
/// taken branch (`conditional_active`).
pub(crate) fn active(stack: &[IfFrame]) -> bool {
    stack.last().is_none_or(|frame| matches!(frame.state, IfState::True | IfState::ElseTrue))
}

/// The expression of a `\if` / `\elif` (`is_true_boolean_expression`):
/// its arguments joined with a space, read as `ParseVariableBool` reads
/// a Boolean. Err: psql's complaint (the branch is then false).
pub(crate) fn expression(args: &[String], command: &str) -> Result<bool, String> {
    let value = args.join(" ");
    crate::psql_vars::parse_bool(&value)
        .ok_or_else(|| format!("unrecognized value \"{value}\" for \"\\{command} expression\": Boolean expected"))
}

/// `branch` applied to `stack`, with `buffer` the query buffer typed
/// before it: the buffer that reads on after it, and what psql says about
/// the expression, if anything. Err: psql's refusal (`\else: no matching
/// \if`); nothing changes.
pub(crate) fn apply(stack: &mut Vec<IfFrame>, branch: Branch, buffer: &str) -> Result<(String, Option<String>), String> {
    use IfState::*;
    let top = stack.last().map(|frame| frame.state);
    let saved = stack.last().map(|frame| frame.saved.clone()).unwrap_or_default();
    // An expression read: its value, and the complaint if it had none.
    let read = |expression: Result<bool, String>| match expression {
        Ok(value) => (value, None),
        Err(said) => (false, Some(said)),
    };
    // The innermost block moves to `state` (saving `buffer`, when given).
    fn set(stack: &mut [IfFrame], state: IfState, buffer: Option<&str>) {
        if let Some(frame) = stack.last_mut() {
            frame.state = state;
            if let Some(buffer) = buffer {
                frame.saved = buffer.to_string();
            }
        }
    }
    Ok(match branch {
        Branch::If(expression) if active(stack) => {
            let (value, said) = read(expression);
            stack.push(IfFrame { state: if value { True } else { False }, saved: buffer.to_string() });
            (buffer.to_string(), said)
        }
        // Under an inactive branch the whole block is ignored, its
        // expression unread.
        Branch::If(_) => {
            stack.push(IfFrame { state: Ignored, saved: buffer.to_string() });
            (buffer.to_string(), None)
        }
        Branch::Elif(expression) => match top {
            // The taken branch ends: what it added stays.
            Some(True) => {
                set(stack, Ignored, Some(buffer));
                (buffer.to_string(), None)
            }
            Some(False) => {
                let restored = saved.clone();
                let (value, said) = read(expression);
                set(stack, if value { True } else { False }, None);
                (restored, said)
            }
            Some(Ignored) => (saved, None),
            Some(ElseTrue | ElseFalse) => return Err("\\elif: cannot occur after \\else".into()),
            None => return Err("\\elif: no matching \\if".into()),
        },
        Branch::Else => match top {
            Some(True) => {
                set(stack, ElseFalse, Some(buffer));
                (buffer.to_string(), None)
            }
            Some(False) => {
                let restored = saved.clone();
                set(stack, ElseTrue, None);
                (restored, None)
            }
            Some(Ignored) => {
                let restored = saved.clone();
                set(stack, ElseFalse, None);
                (restored, None)
            }
            Some(ElseTrue | ElseFalse) => return Err("\\else: cannot occur after \\else".into()),
            None => return Err("\\else: no matching \\if".into()),
        },
        Branch::Endif => match top {
            Some(True | ElseTrue) => {
                stack.pop();
                (buffer.to_string(), None)
            }
            Some(False | Ignored | ElseFalse) => {
                let restored = saved.clone();
                stack.pop();
                (restored, None)
            }
            None => return Err("\\endif: no matching \\if".into()),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use IfState::*;

    fn states(stack: &[IfFrame]) -> Vec<IfState> {
        stack.iter().map(|frame| frame.state).collect()
    }

    #[test]
    fn a_block_takes_the_first_true_branch() {
        let mut stack = Vec::new();
        apply(&mut stack, Branch::If(Ok(false)), "").unwrap();
        assert!(!active(&stack));
        apply(&mut stack, Branch::Elif(Ok(true)), "").unwrap();
        assert!(active(&stack));
        apply(&mut stack, Branch::Elif(Ok(true)), "").unwrap();
        assert_eq!(states(&stack), [Ignored]);
        apply(&mut stack, Branch::Else, "").unwrap();
        assert_eq!(states(&stack), [ElseFalse]);
        apply(&mut stack, Branch::Endif, "").unwrap();
        assert!(stack.is_empty() && active(&stack));
    }

    #[test]
    fn a_block_under_an_inactive_branch_is_ignored_whole() {
        let mut stack = Vec::new();
        apply(&mut stack, Branch::If(Ok(false)), "").unwrap();
        // Its expression isn't read: no complaint.
        assert_eq!(apply(&mut stack, Branch::If(Err("bad".into())), "").unwrap().1, None);
        apply(&mut stack, Branch::Else, "").unwrap();
        assert_eq!(states(&stack), [False, ElseFalse]);
        apply(&mut stack, Branch::Endif, "").unwrap();
        apply(&mut stack, Branch::Else, "").unwrap();
        assert_eq!(states(&stack), [ElseTrue]);
    }

    #[test]
    fn a_bad_expression_is_a_false_branch_and_said() {
        let mut stack = Vec::new();
        let said = apply(&mut stack, Branch::If(expression(&["true".into(), "extra".into()], "if")), "").unwrap().1;
        assert_eq!(said.as_deref(), Some("unrecognized value \"true extra\" for \"\\if expression\": Boolean expected"));
        assert_eq!(states(&stack), [False]);
        assert_eq!(expression(&[], "elif"), Err("unrecognized value \"\" for \"\\elif expression\": Boolean expected".into()));
        assert_eq!(expression(&["t".into()], "if"), Ok(true));
        assert_eq!(expression(&["OFF".into()], "if"), Ok(false));
    }

    #[test]
    fn misplaced_commands_are_refused_and_change_nothing() {
        let mut stack = Vec::new();
        assert_eq!(apply(&mut stack, Branch::Elif(Ok(true)), ""), Err("\\elif: no matching \\if".into()));
        assert_eq!(apply(&mut stack, Branch::Else, ""), Err("\\else: no matching \\if".into()));
        assert_eq!(apply(&mut stack, Branch::Endif, ""), Err("\\endif: no matching \\if".into()));
        apply(&mut stack, Branch::If(Ok(true)), "").unwrap();
        apply(&mut stack, Branch::Else, "").unwrap();
        assert_eq!(apply(&mut stack, Branch::Elif(Ok(true)), ""), Err("\\elif: cannot occur after \\else".into()));
        assert_eq!(apply(&mut stack, Branch::Else, ""), Err("\\else: cannot occur after \\else".into()));
        assert_eq!(states(&stack), [ElseFalse]);
    }

    #[test]
    fn the_buffer_keeps_what_active_branches_add() {
        // `select 1 as a` / `\if false` / `, 2 as b` / `\else` / `, 3 as c`
        // / `\endif` / `;` runs `select 1 as a , 3 as c`, as in psql.
        let mut stack = Vec::new();
        let (buffer, _) = apply(&mut stack, Branch::If(Ok(false)), "select 1 as a").unwrap();
        assert_eq!(buffer, "select 1 as a");
        let (buffer, _) = apply(&mut stack, Branch::Else, "select 1 as a\n, 2 as b").unwrap();
        assert_eq!(buffer, "select 1 as a");
        let (buffer, _) = apply(&mut stack, Branch::Endif, "select 1 as a\n, 3 as c").unwrap();
        assert_eq!(buffer, "select 1 as a\n, 3 as c");
        // An active branch's text stays when it ends; the skipped one's
        // goes.
        let (buffer, _) = apply(&mut stack, Branch::If(Ok(true)), "x").unwrap();
        assert_eq!(buffer, "x");
        let (buffer, _) = apply(&mut stack, Branch::Else, "x y").unwrap();
        assert_eq!(buffer, "x y");
        let (buffer, _) = apply(&mut stack, Branch::Endif, "x y z").unwrap();
        assert_eq!(buffer, "x y");
    }
}
