//! How a group's task command refers to the task it starts.
//!
//! The task's text reaches the command as the value of shell variables the card's staged file
//! exports (`TCANVAS_TASK`, `TCANVAS_TASK_REF`, `TCANVAS_TASK_TITLE`, `TCANVAS_TASK_FILE`), never as
//! text of the command line. A template that names none of the placeholders (`{task}`...) is not
//! looked at: whatever it does with those variables is its own business.
//!
//! A template that names one is accepted only when it is made entirely of constructs this module
//! fully understands (a whitelist, not a parser of everything a shell does): commands made of
//! plain words, single-quoted, double-quoted and backslash-escaped words without a placeholder,
//! simple variable references (`$NAME`, `${NAME}`, `$?`...), environment assignments before a
//! command, the operators `;`, `&&`, `||`, `|` and line breaks, simple redirections to and from
//! file names (`>`, `>>`, `<`, `2>`, `2>&1`...), and a `#` comment after a blank. Anything else
//! refuses it, wherever the placeholder is. A placeholder is expanded only as a whole argument
//! word (`{task}`, `"{task}"`, or `--flag={task}`), never in command position nor as a file name,
//! into its quoted variable (`"$TCANVAS_TASK"`): one argument, whatever the task holds.
//! Commands that evaluate their arguments as code (`eval`, `source`, `let`, `bash -c`...) or
//! arithmetic (`[[`, `[`, `test`) refuse a placeholder too.

/// The placeholders, and the variable each one names.
pub const TASK_PLACEHOLDERS: [(&str, &str); 4] = [("{task}", "TCANVAS_TASK"), ("{task_ref}", "TCANVAS_TASK_REF"), ("{task_title}", "TCANVAS_TASK_TITLE"), ("{task_file}", "TCANVAS_TASK_FILE")];

/// Why a template naming the task is refused: the construct it holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spot {
    /// `$(...)` or backticks.
    CommandSubstitution,
    /// `$((...))`, `((...))`, `$[...]`.
    Arithmetic,
    /// `(...)` or `{ ...; }`.
    Group,
    /// `<<`, `<<<`.
    HereDocument,
    /// `<(...)`, `>(...)`.
    ProcessSubstitution,
    /// `case`, `for`, `while`, `if`, `function`... in command position.
    Keyword,
    /// `eval`, `exec`, `source`, `.`, `let`, `bash -c`... taking a placeholder.
    Evaluating,
    /// `[[`, `[` or `test` taking a placeholder.
    Test,
    /// A placeholder as the command itself.
    CommandPosition,
    /// A placeholder inside quotes with other text, or in `$'...'`.
    Quoted,
    /// A placeholder next to other text (other than `--flag={task}`).
    Glued,
    /// A placeholder as a redirection's file name.
    Redirection,
    /// Anything else (`&`, `${x:-...}`, `$'...'`, an unclosed quote...).
    Unsupported,
}

/// A task command template, its placeholders expanded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskCommand {
    pub command: String,
    /// It names the task at all (a placeholder was expanded).
    pub uses_task: bool,
    /// Why it is refused (the construct's text, and what it is); empty: accepted.
    pub refused: Vec<(String, Spot)>,
}

impl TaskCommand {
    pub fn accepted(&self) -> bool {
        self.refused.is_empty()
    }
}

/// The placeholder starting at `rest`, if any.
fn placeholder(rest: &str) -> Option<(&'static str, &'static str)> {
    TASK_PLACEHOLDERS.iter().copied().find(|(k, _)| rest.starts_with(k))
}

/// One word, as read: what to write back, its unquoted text when it is plain, and whether it holds
/// a placeholder (expanded or not).
struct Word {
    /// The word as written in the template.
    raw: String,
    /// The word's text when it is plain unquoted characters only.
    plain: Option<String>,
    /// It is (or ends in, for `--flag=`) an expanded placeholder.
    expanded: bool,
    /// It holds a placeholder, expanded or refused.
    named: bool,
}

/// Commands that read an argument as code.
const EVALUATING: [&str; 15] = ["eval", "exec", "source", ".", "let", "declare", "typeset", "local", "readonly", "export", "alias", "trap", "builtin", "command", "xargs"];
/// Shells: with `-c`, their argument is code.
const SHELLS: [&str; 6] = ["sh", "bash", "zsh", "dash", "ksh", "fish"];
const KEYWORDS: [&str; 18] = ["case", "esac", "for", "select", "while", "until", "do", "done", "if", "then", "elif", "else", "fi", "function", "time", "coproc", "!", "}"];
const TESTS: [&str; 3] = ["[[", "[", "test"];

fn name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// `--flag=` or `-f=`: a long or short option followed by `=`.
fn flag_prefix(s: &str) -> bool {
    let Some(name) = s.strip_suffix('=') else { return false };
    let name = name.strip_prefix("--").or_else(|| name.strip_prefix('-')).unwrap_or("");
    name.chars().next().is_some_and(|c| c.is_ascii_alphanumeric()) && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Reads the template (see the module). Not fast, not clever: refuses what it does not know.
pub fn expand_task_command(template: &str) -> TaskCommand {
    if !TASK_PLACEHOLDERS.iter().any(|(k, _)| template.contains(k)) {
        return TaskCommand { command: template.to_string(), uses_task: false, refused: Vec::new() };
    }
    let mut s = Scan { chars: template.chars().collect(), i: 0, out: String::new(), refused: Vec::new(), uses_task: false };
    s.run();
    TaskCommand { command: s.out, uses_task: s.uses_task, refused: s.refused }
}

struct Scan {
    chars: Vec<char>,
    i: usize,
    out: String,
    refused: Vec<(String, Spot)>,
    uses_task: bool,
}

impl Scan {
    fn at(&self, k: usize) -> Option<char> {
        self.chars.get(k).copied()
    }

    fn rest(&self) -> String {
        self.chars[self.i..].iter().collect()
    }

    fn refuse(&mut self, what: &str, spot: Spot) {
        if !self.refused.iter().any(|(w, s)| w == what && *s == spot) {
            self.refused.push((what.to_string(), spot));
        }
    }

    /// The whole template: commands separated by operators.
    fn run(&mut self) {
        // Words of the current command: (plain text, holds a placeholder).
        let mut command: Vec<(Option<String>, bool)> = Vec::new();
        let mut in_assignments = true;
        while self.i < self.chars.len() {
            let c = self.chars[self.i];
            match c {
                ' ' | '\t' => {
                    self.out.push(c);
                    self.i += 1;
                }
                '\\' if self.at(self.i + 1) == Some('\n') => {
                    // A line continued: one command.
                    self.out.push_str("\\\n");
                    self.i += 2;
                }
                '\n' | ';' => {
                    if c == ';' && self.at(self.i + 1) == Some(';') {
                        self.refuse(";;", Spot::Keyword);
                    }
                    self.end_command(&mut command, &mut in_assignments);
                    self.out.push(c);
                    self.i += 1;
                }
                '&' | '|' => {
                    let next = self.at(self.i + 1);
                    if next == Some(c) {
                        self.out.push(c);
                        self.out.push(c);
                        self.i += 2;
                    } else if c == '|' && next != Some('&') {
                        self.out.push(c);
                        self.i += 1;
                    } else {
                        // `&` (in the background), `|&`, `&>`...
                        self.refuse(&c.to_string(), Spot::Unsupported);
                        self.out.push(c);
                        self.i += 1;
                    }
                    self.end_command(&mut command, &mut in_assignments);
                }
                '#' => {
                    // After a blank or an operator (a word never starts here otherwise): a comment
                    // to the end of the line, left as it is.
                    while self.i < self.chars.len() && self.chars[self.i] != '\n' {
                        self.out.push(self.chars[self.i]);
                        self.i += 1;
                    }
                }
                '(' | ')' => {
                    let spot = if self.at(self.i + 1) == Some('(') { Spot::Arithmetic } else { Spot::Group };
                    self.refuse(&c.to_string(), spot);
                    self.out.push(c);
                    self.i += 1;
                }
                '<' | '>' => self.redirection(None),
                d if d.is_ascii_digit() && self.redirection_follows() => self.redirection(Some(d)),
                _ => {
                    let word = self.word();
                    if in_assignments && word.assignment() && !word.named {
                        // `NAME=value` before the command.
                        continue;
                    }
                    in_assignments = false;
                    let first = command.is_empty();
                    if first {
                        if word.named {
                            self.refuse("{task}", Spot::CommandPosition);
                        }
                        if let Some(w) = word.plain.as_deref()
                            && (KEYWORDS.contains(&w) || w == "{")
                        {
                            self.refuse(w, if w == "{" || w == "}" { Spot::Group } else { Spot::Keyword });
                        }
                    }
                    command.push((word.plain, word.named || word.expanded));
                }
            }
        }
        self.end_command(&mut command, &mut in_assignments);
    }

    /// The end of a command: what its command word does with the placeholders it takes.
    fn end_command(&mut self, command: &mut Vec<(Option<String>, bool)>, in_assignments: &mut bool) {
        let takes = command.iter().skip(1).any(|(_, named)| *named);
        if let Some((Some(word), _)) = command.first() {
            let base = word.rsplit('/').next().unwrap_or(word);
            if takes && EVALUATING.contains(&base) {
                self.refuse(word, Spot::Evaluating);
            }
            if takes && TESTS.contains(&base) {
                self.refuse(word, Spot::Test);
            }
            if takes && SHELLS.contains(&base) && command.iter().skip(1).any(|(w, _)| w.as_deref().is_some_and(|w| w.starts_with('-') && !w.starts_with("--") && w.contains('c'))) {
                self.refuse(&format!("{word} -c"), Spot::Evaluating);
            }
        } else if !command.is_empty() && takes {
            // A command word that is not plain (quoted, a variable...): what it runs is unknown.
            self.refuse("$command", Spot::Evaluating);
        }
        command.clear();
        *in_assignments = true;
    }

    /// `2>`, `1>&2`... : a digit right before a redirection.
    fn redirection_follows(&self) -> bool {
        let start = self.i == 0 || self.at(self.i - 1).is_some_and(|c| c == ' ' || c == '\t' || c == ';' || c == '|' || c == '&' || c == '\n');
        start && matches!(self.at(self.i + 1), Some('<' | '>'))
    }

    /// A redirection and its file name (or `>&2`).
    fn redirection(&mut self, digit: Option<char>) {
        if let Some(d) = digit {
            self.out.push(d);
            self.i += 1;
        }
        let c = self.chars[self.i];
        let next = self.at(self.i + 1);
        if next == Some('(') {
            self.refuse(&format!("{c}("), Spot::ProcessSubstitution);
            self.out.push(c);
            self.i += 1;
            return;
        }
        if c == '<' && next == Some('<') {
            self.refuse("<<", Spot::HereDocument);
            self.out.push_str("<<");
            self.i += 2;
            return;
        }
        self.out.push(c);
        self.i += 1;
        if c == '>' && next == Some('>') {
            self.out.push('>');
            self.i += 1;
        }
        if self.at(self.i) == Some('&') {
            // `>&2`, `2>&1`.
            self.out.push('&');
            self.i += 1;
            while let Some(d) = self.at(self.i).filter(|c| c.is_ascii_digit() || *c == '-') {
                self.out.push(d);
                self.i += 1;
            }
            return;
        }
        while matches!(self.at(self.i), Some(' ' | '\t')) {
            self.out.push(self.chars[self.i]);
            self.i += 1;
        }
        if self.at(self.i).is_none_or(|c| matches!(c, '\n' | ';' | '&' | '|' | '<' | '>' | '(' | ')')) {
            self.refuse(&c.to_string(), Spot::Unsupported);
            return;
        }
        let word = self.word();
        if word.named {
            self.refuse("{task}", Spot::Redirection);
        }
    }

    /// One word: its pieces up to a blank or an operator.
    fn word(&mut self) -> Word {
        let start = self.i;
        let mut out = String::new();
        let mut plain = Some(String::new());
        let mut expanded = false;
        let mut named = false;
        while let Some(c) = self.at(self.i) {
            if matches!(c, ' ' | '\t' | '\n' | ';' | '&' | '|' | '<' | '>' | '(' | ')') {
                break;
            }
            if let Some((key, var)) = placeholder(&self.rest()) {
                named = true;
                let before: String = self.chars[start..self.i].iter().collect();
                let after = self.at(self.i + key.chars().count());
                let alone = after.is_none_or(|c| matches!(c, ' ' | '\t' | '\n' | ';' | '&' | '|' | '<' | '>'));
                if alone && (self.i == start || (flag_prefix(&before) && plain.is_some())) {
                    out.push_str(&format!("\"${var}\""));
                    expanded = true;
                    self.uses_task = true;
                } else {
                    self.refuse(key, Spot::Glued);
                    out.push_str(key);
                }
                plain = None;
                self.i += key.chars().count();
                continue;
            }
            match c {
                '\'' => {
                    let end = (self.i + 1..self.chars.len()).find(|&k| self.chars[k] == '\'');
                    let Some(end) = end else {
                        self.refuse("'", Spot::Unsupported);
                        out.extend(&self.chars[self.i..]);
                        self.i = self.chars.len();
                        break;
                    };
                    let inner: String = self.chars[self.i + 1..end].iter().collect();
                    if let Some((key, _)) = TASK_PLACEHOLDERS.iter().find(|(k, _)| inner.contains(k)) {
                        named = true;
                        self.refuse(key, Spot::Quoted);
                    }
                    out.extend(&self.chars[self.i..=end]);
                    plain = None;
                    self.i = end + 1;
                }
                '"' => self.double_quoted(start, &mut out, &mut plain, &mut expanded, &mut named),
                '\\' => {
                    out.push(c);
                    if let Some(n) = self.at(self.i + 1) {
                        out.push(n);
                    }
                    plain = None;
                    self.i += 2;
                }
                '`' => {
                    self.refuse("`", Spot::CommandSubstitution);
                    out.push(c);
                    self.i += 1;
                }
                '$' => {
                    if let Some(var) = self.variable() {
                        out.push_str(&var);
                    } else {
                        out.push(c);
                        self.i += 1;
                    }
                    plain = None;
                }
                _ => {
                    out.push(c);
                    if let Some(p) = plain.as_mut() {
                        p.push(c);
                    }
                    self.i += 1;
                }
            }
        }
        let raw = self.chars[start..self.i].iter().collect();
        self.out.push_str(&out);
        Word { raw, plain, expanded, named }
    }

    /// `"..."`: no placeholder inside unless it is the whole word (`"{task}"`).
    fn double_quoted(&mut self, start: usize, out: &mut String, plain: &mut Option<String>, expanded: &mut bool, named: &mut bool) {
        let open = self.i;
        self.i += 1;
        let inner_start = self.i;
        let mut inner = String::new();
        loop {
            let Some(c) = self.at(self.i) else {
                self.refuse("\"", Spot::Unsupported);
                out.extend(&self.chars[open..]);
                return;
            };
            match c {
                '"' => break,
                '\\' => {
                    inner.push(c);
                    if let Some(n) = self.at(self.i + 1) {
                        inner.push(n);
                    }
                    self.i += 2;
                }
                '`' => {
                    self.refuse("`", Spot::CommandSubstitution);
                    inner.push(c);
                    self.i += 1;
                }
                '$' => match self.variable() {
                    Some(v) => inner.push_str(&v),
                    None => {
                        inner.push(c);
                        self.i += 1;
                    }
                },
                _ => {
                    inner.push(c);
                    self.i += 1;
                }
            }
        }
        self.i += 1;
        let raw: String = self.chars[inner_start..self.i - 1].iter().collect();
        let after = self.at(self.i);
        let alone = open == start && after.is_none_or(|c| matches!(c, ' ' | '\t' | '\n' | ';' | '&' | '|' | '<' | '>'));
        if let Some((key, var)) = placeholder(&raw).filter(|(k, _)| raw == *k) {
            *named = true;
            if alone {
                out.push_str(&format!("\"${var}\""));
                *expanded = true;
                self.uses_task = true;
            } else {
                self.refuse(key, Spot::Glued);
                out.push('"');
                out.push_str(&inner);
                out.push('"');
            }
        } else {
            if let Some((key, _)) = TASK_PLACEHOLDERS.iter().find(|(k, _)| raw.contains(k)) {
                *named = true;
                self.refuse(key, Spot::Quoted);
            }
            out.push('"');
            out.push_str(&inner);
            out.push('"');
        }
        *plain = None;
    }

    /// `$NAME`, `${NAME}`, `$?`, `$1`...: the reference as written. Anything else after `$`
    /// (`$(`, `$((`, `$[`, `$'`, `${x:-...}`) is refused (and `None` returned).
    fn variable(&mut self) -> Option<String> {
        let next = self.at(self.i + 1);
        match next {
            Some('(') => {
                let spot = if self.at(self.i + 2) == Some('(') { Spot::Arithmetic } else { Spot::CommandSubstitution };
                self.refuse(if spot == Spot::Arithmetic { "$((" } else { "$(" }, spot);
                None
            }
            Some('[') => {
                self.refuse("$[", Spot::Arithmetic);
                None
            }
            Some('{') => {
                let end = (self.i + 2..self.chars.len()).find(|&k| self.chars[k] == '}');
                match end {
                    Some(end) if end > self.i + 2 && self.chars[self.i + 2..end].iter().all(|c| name_char(*c)) => {
                        let v: String = self.chars[self.i..=end].iter().collect();
                        self.i = end + 1;
                        Some(v)
                    }
                    _ => {
                        self.refuse("${", Spot::Unsupported);
                        None
                    }
                }
            }
            Some(c) if name_char(c) => {
                let mut k = self.i + 1;
                while self.at(k).is_some_and(name_char) {
                    k += 1;
                }
                let v: String = self.chars[self.i..k].iter().collect();
                self.i = k;
                Some(v)
            }
            Some(c) if matches!(c, '?' | '#' | '@' | '*' | '!' | '$' | '-') => {
                self.i += 2;
                Some(format!("${c}"))
            }
            Some('\'') | Some('"') => {
                self.refuse("$'", Spot::Unsupported);
                None
            }
            // A lone `$`: itself.
            _ => {
                self.i += 1;
                Some("$".into())
            }
        }
    }
}

impl Word {
    /// `NAME=value` (before the command: an environment assignment).
    fn assignment(&self) -> bool {
        self.raw.split_once('=').is_some_and(|(n, _)| !n.is_empty() && n.chars().all(name_char) && !n.starts_with(|c: char| c.is_ascii_digit()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(t: &str) -> Vec<Spot> {
        expand_task_command(t).refused.into_iter().map(|(_, s)| s).collect()
    }

    #[test]
    fn templates_without_a_placeholder_are_not_looked_at() {
        for own in ["bash -c 'claude \"$TCANVAS_TASK\"'", "claude \"$(cat \"$TCANVAS_TASK_FILE\")\"", "for x in a; do echo $x; done", "( cd x && y ) &"] {
            assert_eq!(expand_task_command(own), TaskCommand { command: own.into(), uses_task: false, refused: vec![] }, "{own}");
        }
    }

    /// Templates the scanner accepts, and what they become.
    pub const ACCEPTED: [(&str, &str); 14] = [
        ("claude {task}", "claude \"$TCANVAS_TASK\""),
        ("claude \"{task}\"", "claude \"$TCANVAS_TASK\""),
        ("claude --prompt={task}", "claude --prompt=\"$TCANVAS_TASK\""),
        (
            "agent --ref {task_ref} --title {task_title} --file {task_file} -- {task}",
            "agent --ref \"$TCANVAS_TASK_REF\" --title \"$TCANVAS_TASK_TITLE\" --file \"$TCANVAS_TASK_FILE\" -- \"$TCANVAS_TASK\"",
        ),
        ("cd sub && claude {task}; echo done", "cd sub && claude \"$TCANVAS_TASK\"; echo done"),
        ("A=1 B=\"x y\" claude {task} || echo failed", "A=1 B=\"x y\" claude \"$TCANVAS_TASK\" || echo failed"),
        ("claude {task} 2>&1 | tee log.txt", "claude \"$TCANVAS_TASK\" 2>&1 | tee log.txt"),
        ("claude {task} > out.txt 2> err.txt < /dev/null", "claude \"$TCANVAS_TASK\" > out.txt 2> err.txt < /dev/null"),
        ("claude 'it''s' \"a $HOME ${USER}\" \\{x\\} $1 {task} # {task} in a comment", "claude 'it''s' \"a $HOME ${USER}\" \\{x\\} $1 \"$TCANVAS_TASK\" # {task} in a comment"),
        ("npm test\nclaude {task}", "npm test\nclaude \"$TCANVAS_TASK\""),
        ("claude \\\n  {task}", "claude \\\n  \"$TCANVAS_TASK\""),
        ("/usr/bin/claude -p {task}", "/usr/bin/claude -p \"$TCANVAS_TASK\""),
        ("codex exec {task_file}", "codex exec \"$TCANVAS_TASK_FILE\""),
        ("exec_agent -c {task}", "exec_agent -c \"$TCANVAS_TASK\""),
    ];

    #[test]
    fn accepted_templates_expand_each_placeholder_to_its_quoted_variable() {
        for (template, expanded) in ACCEPTED {
            let e = expand_task_command(template);
            assert!(e.accepted(), "{template:?}: {:?}", e.refused);
            assert_eq!(e.command, expanded, "{template:?}");
            assert!(e.uses_task);
        }
    }

    #[test]
    fn anything_else_refuses_a_template_naming_the_task() {
        use Spot::*;
        let cases: &[(&str, Spot)] = &[
            // The review's: a `)` of a case, a here-document, a comment inside a substitution,
            // arithmetic and tests.
            ("printf '<%s>' $(case x in x) printf %s {task};; esac)", CommandSubstitution),
            ("printf '<%s>' $(cat <<EOF\n) {task}\nEOF\n)", CommandSubstitution),
            ("printf '<%s>' $(printf %s # )\n{task})", CommandSubstitution),
            ("(( {task} ))", Arithmetic),
            ("[[ {task} -eq 0 ]]", Test),
            ("[ {task} -eq 0 ]", Test),
            ("test {task} = x", Test),
            ("echo $(( {task} ))", Arithmetic),
            ("echo $[ {task} ]", Arithmetic),
            ("bash -c \"claude {task}\"", Quoted),
            ("ssh host \"claude {task}\"", Quoted),
            ("claude '{task}'", Quoted),
            ("claude $'{task}'", Unsupported),
            ("claude \"$(printf %s {task})\"", CommandSubstitution),
            ("claude $(printf %s {task})", CommandSubstitution),
            ("claude `printf %s {task}`", CommandSubstitution),
            ("cat <<EOF\n{task}\nEOF", HereDocument),
            ("claude <<< {task}", HereDocument),
            ("diff <(echo {task}) x", ProcessSubstitution),
            ("(claude {task})", Group),
            ("{ claude {task}; }", Group),
            ("for x in {task}; do claude $x; done", Keyword),
            ("if true; then claude {task}; fi", Keyword),
            ("case x in *) claude {task};; esac", Group),
            ("eval {task}", Evaluating),
            ("exec {task}", Evaluating),
            ("source {task_file}", Evaluating),
            (". {task_file}", Evaluating),
            ("let x={task}", Evaluating),
            ("bash -c {task}", Evaluating),
            ("sh -ec {task}", Evaluating),
            ("\"$CMD\" {task}", Evaluating),
            ("{task}", CommandPosition),
            ("X=1 {task} --help", CommandPosition),
            ("claude x{task}", Glued),
            ("claude {task}x", Glued),
            ("claude X={task}", Glued),
            ("claude \"a {task}\"", Quoted),
            ("claude \"{task}\"x", Glued),
            ("claude {task} > {task_ref}.txt", Redirection),
            ("claude {task} &", Unsupported),
            ("claude ${task:-x} {task}", Unsupported),
            ("claude \"unclosed {task}", Unsupported),
        ];
        for (template, spot) in cases {
            let e = expand_task_command(template);
            assert!(e.refused.iter().any(|(_, s)| s == spot), "{template:?}: {:?}", e.refused);
        }
        assert!(refused("claude {task}").is_empty());
    }
}

/// Runs templates for real (`bash`, `zsh`): accepted ones must hand the task's value over as
/// exactly the arguments the placeholders stand for, whatever it holds, and run nothing of it.
#[cfg(all(test, unix))]
mod live {
    use super::*;
    use std::path::PathBuf;
    use std::process::Command;

    /// A value written to hurt.
    const HOSTILE: &str = "a b  $(touch pwned1) `touch pwned2` \"q\" 'q' * ~ ? [x] \\ ;|& $HOME\nnext line a[$(touch pwned3)] -x --y {task}";
    /// What `rec` records: each argument, ended by a NUL.
    const PRELUDE: &str = "rec() { for a in \"$@\"; do printf '%s\\0' \"$a\"; done >> \"$ARGS\"; }\n";

    fn shells() -> Vec<String> {
        ["bash", "zsh"].into_iter().filter_map(|s| ["/usr/bin/", "/bin/"].iter().map(|d| format!("{d}{s}")).find(|p| std::path::Path::new(p).exists())).collect()
    }

    struct Dir(PathBuf);

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Runs `template` (accepted) in `shell` with the variables set; the recorded arguments.
    fn run(shell: &str, template: &str, n: usize) -> Vec<String> {
        let e = expand_task_command(template);
        assert!(e.accepted(), "{template:?}: {:?}", e.refused);
        let dir = Dir(std::env::temp_dir().join(format!("tcanvas-task-command-{}-{n}", std::process::id())));
        let _ = std::fs::remove_dir_all(&dir.0);
        std::fs::create_dir_all(dir.0.join("sub")).unwrap();
        let status = Command::new(shell)
            .args(["-c", &format!("{PRELUDE}{}", e.command)])
            .current_dir(&dir.0)
            .env("TCANVAS_TASK", HOSTILE)
            .env("TCANVAS_TASK_REF", "k3f")
            .env("TCANVAS_TASK_TITLE", HOSTILE)
            .env("TCANVAS_TASK_FILE", "/tmp/x y/task")
            .env("HOME", "/h")
            .env("ARGS", dir.0.join("args"))
            .status()
            .unwrap();
        let _ = status;
        let pwned: Vec<String> = std::fs::read_dir(&dir.0).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with("pwned")).collect();
        assert!(pwned.is_empty(), "{shell} {template:?}: the value ran: {pwned:?}");
        let args = std::fs::read(dir.0.join("args")).unwrap_or_default();
        String::from_utf8(args).unwrap().split_terminator('\0').map(String::from).collect()
    }

    #[test]
    fn accepted_templates_hand_the_value_over_as_one_unchanged_argument() {
        let cases: [(&str, Vec<String>); 7] = [
            ("rec {task}", vec![HOSTILE.into()]),
            ("rec \"{task}\" {task_ref}", vec![HOSTILE.into(), "k3f".into()]),
            ("rec --prompt={task} -t {task_title}", vec![format!("--prompt={HOSTILE}"), "-t".into(), HOSTILE.into()]),
            ("A=1 rec 'x y' \"$HOME\" \\* {task} > /dev/null; true", vec!["x y".into(), "/h".into(), "*".into(), HOSTILE.into()]),
            ("cd sub && rec {task_file} 2>&1 | cat > /dev/null # {task}", vec!["/tmp/x y/task".into()]),
            ("false || rec {task}\nrec {task_ref}", vec![HOSTILE.into(), "k3f".into()]),
            ("rec \\\n {task}", vec![HOSTILE.into()]),
        ];
        let mut n = 0;
        for shell in shells() {
            for (template, want) in &cases {
                n += 1;
                assert_eq!(&run(&shell, template, n), want, "{shell} {template:?}");
            }
        }
    }

    /// Random templates made of the allowed constructs around placeholders: all accepted, all
    /// handing the value over unchanged.
    #[test]
    fn random_combinations_of_allowed_constructs_hand_the_value_over() {
        let prefixes: [(&str, bool); 4] = [("", false), ("A=1 ", false), ("true && ", false), ("false || ", false)];
        let args: [(&str, Option<&str>); 8] =
            [("plain", Some("plain")), ("'s q'", Some("s q")), ("\"d q\"", Some("d q")), ("$HOME", Some("/h")), ("\\x", Some("x")), ("{task}", None), ("\"{task}\"", None), ("--p={task}", None)];
        let suffixes = ["", " > /dev/null", " 2>&1", " | cat > /dev/null", "; true", " # c {task}"];
        let mut seed: u64 = 0x7a5c;
        let mut next = move |n: usize| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((seed >> 33) as usize) % n
        };
        let shells = shells();
        for k in 0..120 {
            let (prefix, _) = prefixes[next(prefixes.len())];
            let mut template = format!("{prefix}rec");
            let mut want = Vec::new();
            let count = 1 + next(4);
            for _ in 0..count {
                let (arg, value) = args[next(args.len())];
                template.push(' ');
                template.push_str(arg);
                want.push(match value {
                    Some(v) => v.to_string(),
                    None if arg.starts_with("--p=") => format!("--p={HOSTILE}"),
                    None => HOSTILE.to_string(),
                });
            }
            if !template.contains("{task}") {
                template.push_str(" {task}");
                want.push(HOSTILE.into());
            }
            template.push_str(suffixes[next(suffixes.len())]);
            for shell in &shells {
                assert_eq!(run(shell, &template, 1000 + k), want, "{shell} {template:?}");
            }
        }
    }
}
