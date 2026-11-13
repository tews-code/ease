//! Parse shell command arguments

use alloc::vec::Vec;

/// The number of parsed items (e.g flags) that EASE supports
const COMMAND_DEPTH: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgsError<'a> {
    /// The command has too many characters
    CommandTooLong,
    /// A command with a flag has the flag but not the flag's value
    FlagMissingValue,
    /// An unknown flag has been found
    UnknownFlag(&'a str),
}

/// Struct holding flags and positional arguments
pub struct Args<'a> {
    flags: Vec<&'a str>,
    flag_values: Vec<(&'a str, &'a str)>,
    pub positionals: Vec<&'a str>,
    rest: &'a str,
}

impl Args<'_> {
    pub fn has_flag(&self, flag: &str) -> bool {
        self.flags.contains(&flag)
    }

    pub fn flag_value(&self, flag: &str) -> Option<&str> {
        self.flag_values
            .iter()
            .find(|fv| fv.0 == flag)
            .map(|v| v.1)
    }

    pub fn parse<'a>(
        rest: &'a str,
        flag_set: &[&str],
        value_flag_set: &[&str],
    ) -> Result<Args<'a>, ArgsError<'a>> {
        let mut flags = Vec::<&str>::with_capacity(COMMAND_DEPTH);
        let mut flag_values = Vec::<(&str, &str)>::with_capacity(COMMAND_DEPTH);
        let mut positionals = Vec::<&str>::with_capacity(COMMAND_DEPTH);
        let mut tokens = rest.split_whitespace();
        while let Some(token) = tokens.next() {
            if let Some(flag_chars) = token.strip_prefix('-') {
                // Check if this is a value flag
                if value_flag_set.contains(&flag_chars) {
                    if let Some(value) = tokens.next() {
                        if flag_values.len() == COMMAND_DEPTH {
                            return Err(ArgsError::CommandTooLong);
                        } else {
                            flag_values.push((flag_chars, value))
                        }
                    } else {
                        return Err(ArgsError::FlagMissingValue);
                    }
                } else {
                    // This must be just a flag - check validitity
                    for (i, ch) in flag_chars.char_indices() {
                        let flag_char = &flag_chars[i..i + ch.len_utf8()];
                        if flag_set.contains(&flag_char) {
                            if flags.len() == COMMAND_DEPTH {
                                return Err(ArgsError::CommandTooLong);
                            } else {
                                flags.push(flag_char)
                            }
                        } else {
                            return Err(ArgsError::UnknownFlag(flag_char));
                        }
                    }
                }
            } else {
                // The rest of the tokens are positionals
                if positionals.len() == COMMAND_DEPTH {
                    return Err(ArgsError::CommandTooLong);
                } else {
                    positionals.push(token)
                }
            }
        }
        Ok(Args {
            flags,
            flag_values,
            positionals,
            rest,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty_input() {
        let args = Args::parse("", &[], &[]);
        assert_eq!(args.as_ref().unwrap().flags.len(), 0);
        assert_eq!(args.as_ref().unwrap().positionals.len(), 0);
    }

    #[test]
    fn parse_single_positional() {
        let args = Args::parse("HELLO.TXT", &[], &[]);
        assert_eq!(args.as_ref().unwrap().flags.len(), 0);
        assert_eq!(args.as_ref().unwrap().positionals.len(), 1);
        assert_eq!(args.as_ref().unwrap().positionals[0], "HELLO.TXT");
    }

    #[test]
    fn parse_multiple_positionals() {
        let args = Args::parse("FOO.TXT BAR.TXT", &[], &[]);
        assert_eq!(args.as_ref().unwrap().positionals.len(), 2);
        assert_eq!(args.as_ref().unwrap().positionals[0], "FOO.TXT");
        assert_eq!(args.as_ref().unwrap().positionals[1], "BAR.TXT");
    }

    #[test]
    fn parse_single_flag() {
        let args = Args::parse("-l", &["l"], &[]);
        assert_eq!(args.as_ref().unwrap().flags.len(), 1);
        assert!(args.as_ref().unwrap().has_flag("l"));
        assert_eq!(args.as_ref().unwrap().positionals.len(), 0);
    }

    #[test]
    fn parse_multiple_separate_flags() {
        let args = Args::parse("-l -a", &["l", "a"], &[]);
        assert_eq!(args.as_ref().unwrap().flags.len(), 2);
        assert!(args.as_ref().unwrap().has_flag("l"));
        assert!(args.as_ref().unwrap().has_flag("a"));
    }

    #[test]
    fn parse_combined_flags() {
        let args = Args::parse("-la", &["l", "a"], &[]);
        assert_eq!(args.as_ref().unwrap().flags.len(), 2);
        assert!(args.as_ref().unwrap().has_flag("l"));
        assert!(args.as_ref().unwrap().has_flag("a"));
    }

    #[test]
    fn parse_flags_and_positionals_mixed() {
        let args = Args::parse("-l HELLO.TXT -a", &["l", "a"], &[]);
        assert!(args.as_ref().unwrap().has_flag("l"));
        assert!(args.as_ref().unwrap().has_flag("a"));
        assert_eq!(args.as_ref().unwrap().positionals.len(), 1);
        assert_eq!(args.as_ref().unwrap().positionals[0], "HELLO.TXT");
    }

    #[test]
    fn has_flag_returns_false_for_absent_flag() {
        let args = Args::parse("-l", &["l"], &[]);
        assert!(!args.as_ref().unwrap().has_flag("a"));
    }

    #[test]
    fn parse_whitespace_only() {
        let args = Args::parse("   ", &[], &[]);
        assert_eq!(args.as_ref().unwrap().flags.len(), 0);
        assert_eq!(args.as_ref().unwrap().positionals.len(), 0);
    }

    // --- value flags and flag validation ---

    #[test]
    fn parse_value_flag() {
        let args = Args::parse("-s 1024", &[], &["s"]);
        assert_eq!(args.as_ref().unwrap().flag_value("s"), Some("1024"));
        // A value flag is not also recorded as a boolean flag.
        assert_eq!(args.as_ref().unwrap().flags.len(), 0);
        assert_eq!(args.as_ref().unwrap().positionals.len(), 0);
    }

    #[test]
    fn parse_value_flag_then_positional() {
        let args = Args::parse("-s 1024 FILE.BIN", &["C"], &["s"]);
        assert_eq!(args.as_ref().unwrap().flag_value("s"), Some("1024"));
        assert_eq!(args.as_ref().unwrap().positionals.len(), 1);
        assert_eq!(args.as_ref().unwrap().positionals[0], "FILE.BIN");
    }

    #[test]
    fn flag_value_returns_none_when_absent() {
        let args = Args::parse("-C", &["C"], &["s"]);
        assert_eq!(args.as_ref().unwrap().flag_value("s"), None);
    }

    #[test]
    fn parse_unknown_flag_rejected() {
        let args = Args::parse("-x", &[], &[]);
        assert!(matches!(args, Err(ArgsError::UnknownFlag("x"))));
    }

    #[test]
    fn parse_value_flag_missing_value() {
        let args = Args::parse("-s", &[], &["s"]);
        assert!(matches!(args, Err(ArgsError::FlagMissingValue)));
    }
}
