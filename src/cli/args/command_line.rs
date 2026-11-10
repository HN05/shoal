//! Writing a command's parsed arguments back as its command line, each flag
//! spelled as its clap definition gives it, so that the line parses to the
//! same arguments.
use std::ffi::OsString;

/// The `shoal` arguments of one command, written argument by argument.
/// Each argument is named by its clap ID, the field that holds it, and is
/// looked up whether or not it has a value, so a misnamed one fails every
/// test that writes the command.
pub(in crate::cli) struct CommandLine {
    definition: clap::Command,
    positionals: Vec<OsString>,
    flags: Vec<OsString>,
    trailing: Vec<OsString>,
}

impl CommandLine {
    /// The command line of `shoal <subcommand...>`, whose arguments `A`
    /// defines.
    pub fn of<A: clap::Args>(subcommand: &[&str]) -> Self {
        Self {
            definition: A::augment_args(clap::Command::new("shoal")),
            positionals: subcommand.iter().map(OsString::from).collect(),
            flags: Vec::new(),
            trailing: Vec::new(),
        }
    }

    /// The flag `id` alone when `on`.
    pub fn switch(&mut self, id: &str, on: bool) {
        let flag = self.flag(id);
        if on {
            self.flags.push(flag);
        }
    }

    /// The flag `id` with each of `values`, once for an optional value.
    pub fn values<V: Into<OsString>>(&mut self, id: &str, values: impl IntoIterator<Item = V>) {
        let flag = self.flag(id);
        for value in values {
            self.flags.extend([flag.clone(), value.into()]);
        }
    }

    /// The flag `id`, whose value is optional: alone for `Some(None)`.
    pub fn optional<V: Into<OsString>>(&mut self, id: &str, value: Option<Option<V>>) {
        let flag = self.flag(id);
        match value {
            None => {}
            // Flags follow every positional, so the next one cannot be
            // taken for the missing value.
            Some(None) => self.flags.push(flag),
            Some(Some(value)) => self.flags.extend([flag, value.into()]),
        }
    }

    /// The positional argument `id`, after those written before it.
    pub fn positional(&mut self, id: &str, value: Option<impl Into<OsString>>) {
        let argument = self.argument(id);
        assert!(argument.is_positional(), "{id} is not positional");
        self.positionals.extend(value.map(Into::into));
    }

    /// The arguments `id` takes after `--`.
    pub fn trailing(&mut self, id: &str, values: &[OsString]) {
        let argument = self.argument(id);
        assert!(argument.is_last_set(), "{id} does not follow --");
        self.trailing.extend_from_slice(values);
    }

    /// The written arguments, after `shoal`.
    pub fn into_args(self) -> Vec<OsString> {
        let mut args = [self.positionals, self.flags].concat();
        if !self.trailing.is_empty() {
            args.push("--".into());
            args.extend(self.trailing);
        }
        args
    }

    /// `--<long>` of the flag `id`.
    fn flag(&self, id: &str) -> OsString {
        let long =
            (self.argument(id).get_long()).unwrap_or_else(|| panic!("{id} has no long flag"));
        format!("--{long}").into()
    }

    /// The definition of the argument `id`.
    fn argument(&self, id: &str) -> &clap::Arg {
        (self.definition.get_arguments())
            .find(|argument| argument.get_id() == id)
            .unwrap_or_else(|| panic!("no argument {id}"))
    }
}
