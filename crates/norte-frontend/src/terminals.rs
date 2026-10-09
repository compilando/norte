//! The terminal panel's instances: several shells in ONE slot, VS Code
//! style (spec `2026-10-09-terminal-instances-design.md`).
//!
//! Pure: generic over the shell, so the rules are tested without a pty and
//! both frontends obey the same ones (ADR 0077).

/// The commands whose lone chord still reaches norte while the terminal
/// panel holds the keyboard; every other chord is the shell's. Shared by
/// both frontends (ADR 0077), and the exit chord (`layout.terminal`) is
/// checked on its own.
///
/// SHORT on purpose. `layout.close-slot` is NOT here: its `alt+x` is Emacs's
/// `M-x`, and closing the slot kills every shell in it. Nor are the other
/// panels' toggles: `alt+l` and `alt+t` are readline words.
pub const PASS_THROUGH: &[&str] = &[
    "layout.focus-next",
    "layout.focus-prev",
    "terminal.new",
    "terminal.new-profile",
    "terminal.close",
    "terminal.next",
    "terminal.prev",
    "terminal.rename",
    "terminal.decorate",
];

/// An instance's identity: monotonic, never reused within a session, so a
/// click aimed at a closed instance cannot land on its successor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InstanceId(pub u32);

/// An instance's colour: an ANSI index from 1 to 6, which the THEME
/// resolves — never a `#hex`, or the mark would stop obeying the theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnsiColor(u8);

impl AnsiColor {
    /// `None` outside 1..=6 (0 and 7 are black and white: not a mark).
    ///
    /// ```
    /// use norte_frontend::terminals::AnsiColor;
    /// assert_eq!(AnsiColor::new(4).map(AnsiColor::index), Some(4));
    /// assert!(AnsiColor::new(7).is_none());
    /// ```
    #[must_use]
    pub fn new(index: u8) -> Option<Self> {
        (1..=6).contains(&index).then_some(Self(index))
    }

    /// The ANSI index.
    #[must_use]
    pub fn index(self) -> u8 {
        self.0
    }
}

/// The fixed set of icons an instance can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalIcon {
    /// A prompt.
    Terminal,
    /// Source code.
    Code,
    /// A server.
    Server,
    /// A bug.
    Debug,
    /// A package.
    Package,
    /// A star.
    Star,
}

impl TerminalIcon {
    /// Every icon, in menu order.
    pub const ALL: [Self; 6] = [
        Self::Terminal,
        Self::Code,
        Self::Server,
        Self::Debug,
        Self::Package,
        Self::Star,
    ];

    /// The spelling in `terminal.toml` and on the bridge.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Terminal => "terminal",
            Self::Code => "code",
            Self::Server => "server",
            Self::Debug => "debug",
            Self::Package => "package",
            Self::Star => "star",
        }
    }

    /// The inverse of [`Self::as_str`].
    ///
    /// ```
    /// use norte_frontend::terminals::TerminalIcon;
    /// assert_eq!(TerminalIcon::parse("server"), Some(TerminalIcon::Server));
    /// assert_eq!(TerminalIcon::parse("rocket"), None);
    /// ```
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|i| i.as_str() == s)
    }
}

/// What [`Terminals::tick`] needs from a shell. A trait so the rules are
/// tested without a pty; each frontend implements it over `norte-term`'s.
pub trait TerminalShell {
    /// Feeds what the shell wrote into its grid; `true` if there was any.
    fn pump(&mut self) -> bool;
    /// Fits the grid and the pty to the slot.
    fn resize(&mut self, size: (u16, u16));
    /// The title the program set since the last call.
    fn take_title(&mut self) -> Option<String>;
    /// `Some(code)` once the shell ended.
    fn exit_code(&mut self) -> Option<i32>;
}

/// What one [`Terminals::tick`] changed, so the frontend repaints only
/// what it must.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TickReport {
    /// The grid in front changed (output, or a new instance came forward).
    pub active_output: bool,
    /// The list changed: a title, an unseen mark, an exit.
    pub list_changed: bool,
}

/// One shell of the panel, with what the reader sees of it.
#[derive(Debug)]
pub struct Instance<S> {
    /// Its identity.
    pub id: InstanceId,
    /// The shell profile it was started from.
    pub profile: String,
    /// The name the reader gave it; wins over [`Self::title`].
    pub name: Option<String>,
    /// The last title the program set (OSC 0/2), already sanitised.
    pub title: Option<String>,
    /// Its icon.
    pub icon: Option<TerminalIcon>,
    /// Its colour.
    pub color: Option<AnsiColor>,
    /// `Some(code)` once the shell is gone.
    pub exited: Option<i32>,
    /// Output arrived while another instance was in front.
    pub unseen: bool,
    /// The shell. Kept after a non-zero exit: dead, it still holds the last
    /// screen the reader has to see.
    pub shell: S,
}

/// The panel's instances and which one is in front.
#[derive(Debug)]
pub struct Terminals<S> {
    instances: Vec<Instance<S>>,
    active: Option<InstanceId>,
    next_id: u32,
}

impl<S> Default for Terminals<S> {
    fn default() -> Self {
        Self {
            instances: Vec::new(),
            active: None,
            next_id: 1,
        }
    }
}

impl<S> Terminals<S> {
    /// No instances.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// No instances at all (not even exited ones)?
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.instances.is_empty()
    }

    /// How many instances, exited ones included.
    #[must_use]
    pub fn len(&self) -> usize {
        self.instances.len()
    }

    /// The instances, in list order.
    pub fn iter(&self) -> impl Iterator<Item = &Instance<S>> {
        self.instances.iter()
    }

    /// The instances, in list order, mutably.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Instance<S>> {
        self.instances.iter_mut()
    }

    /// The one in front.
    #[must_use]
    pub fn active(&self) -> Option<&Instance<S>> {
        self.active.and_then(|id| self.get(id))
    }

    /// The one in front, mutably.
    pub fn active_mut(&mut self) -> Option<&mut Instance<S>> {
        let id = self.active?;
        self.instances.iter_mut().find(|i| i.id == id)
    }

    /// Which one is in front.
    #[must_use]
    pub fn active_id(&self) -> Option<InstanceId> {
        self.active
    }

    /// Appends an instance at the end and puts it in front.
    pub fn push(
        &mut self,
        profile: String,
        icon: Option<TerminalIcon>,
        color: Option<AnsiColor>,
        shell: S,
    ) -> InstanceId {
        let id = InstanceId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        self.instances.push(Instance {
            id,
            profile,
            name: None,
            title: None,
            icon,
            color,
            exited: None,
            unseen: false,
            shell,
        });
        self.active = Some(id);
        id
    }

    /// Removes an instance (dropping its shell kills it). If it was in
    /// front, its right neighbour takes its place, else its left one.
    pub fn close(&mut self, id: InstanceId) -> Option<Instance<S>> {
        let pos = self.position(id)?;
        let gone = self.instances.remove(pos);
        if self.active == Some(id) {
            self.active = None;
            let next = self
                .instances
                .get(pos)
                .or_else(|| pos.checked_sub(1).and_then(|p| self.instances.get(p)))
                .map(|i| i.id);
            // Through `select`: the one coming forward is now being seen.
            if let Some(next) = next {
                self.select(next);
            }
        }
        Some(gone)
    }

    /// Hands every instance out, emptying the list — so the caller decides
    /// WHERE their shells die (killing one can block).
    pub fn drain(&mut self) -> Vec<Instance<S>> {
        self.active = None;
        std::mem::take(&mut self.instances)
    }

    /// Puts it in front; `false` if there is no such instance.
    pub fn select(&mut self, id: InstanceId) -> bool {
        let Some(i) = self.get_mut(id) else {
            return false;
        };
        i.unseen = false;
        self.active = Some(id);
        true
    }

    /// The next one, wrapping.
    pub fn next(&mut self) {
        self.step(1);
    }

    /// The previous one, wrapping.
    pub fn prev(&mut self) {
        self.step(self.instances.len().saturating_sub(1));
    }

    /// Names it; a name blank after trimming clears the name. It comes from
    /// a renderer, so it is cleaned like a program's title: no control or
    /// bidi characters, at most 128 characters.
    pub fn rename(&mut self, id: InstanceId, name: &str) {
        if let Some(i) = self.get_mut(id) {
            let name: String = name
                .chars()
                .filter(|c| {
                    !c.is_control()
                        && !matches!(
                            c,
                            '\u{61c}'
                                | '\u{200e}'
                                | '\u{200f}'
                                | '\u{2028}'
                                | '\u{2029}'
                                | '\u{202a}'..='\u{202e}'
                                | '\u{2066}'..='\u{2069}'
                        )
                })
                .take(128)
                .collect();
            let name = name.trim();
            i.name = (!name.is_empty()).then(|| name.to_owned());
        }
    }

    /// Sets (or clears) its icon and colour.
    pub fn decorate(
        &mut self,
        id: InstanceId,
        icon: Option<TerminalIcon>,
        color: Option<AnsiColor>,
    ) {
        if let Some(i) = self.get_mut(id) {
            i.icon = icon;
            i.color = color;
        }
    }

    /// Its shell ended. A clean exit (0) removes it; any other code keeps
    /// it, without a shell, so the reader sees how it ended.
    pub fn on_exit(&mut self, id: InstanceId, code: i32) {
        if code == 0 {
            self.close(id);
        } else if let Some(i) = self.get_mut(id) {
            // The dead shell stays: it holds the last screen.
            i.exited = Some(code);
        }
    }

    /// Its shell wrote: marked unseen unless it is the one in front.
    pub fn on_output(&mut self, id: InstanceId) {
        if self.active == Some(id) {
            return;
        }
        if let Some(i) = self.get_mut(id) {
            i.unseen = true;
        }
    }

    /// The program set a title; an empty one clears it.
    pub fn set_title(&mut self, id: InstanceId, title: String) {
        if let Some(i) = self.get_mut(id) {
            i.title = (!title.is_empty()).then_some(title);
        }
    }

    /// What its tab says: the reader's name, else the program's title, else
    /// the shell profile.
    #[must_use]
    pub fn display_title(&self, id: InstanceId) -> Option<&str> {
        let i = self.get(id)?;
        Some(
            i.name
                .as_deref()
                .or(i.title.as_deref())
                .unwrap_or(&i.profile),
        )
    }

    /// Drops every instance and its shell (the panel closed).
    pub fn clear(&mut self) {
        self.instances.clear();
        self.active = None;
    }

    /// One turn of every live shell: resize (`None` = the slot is not
    /// placed, keep the last good size), pump, collect title and exit.
    ///
    /// EVERY shell, not only the one in front: the pty keeps only a tail of
    /// what nobody pumped, so a shell left behind would come back with a
    /// corrupted screen.
    pub fn tick(&mut self, size: Option<(u16, u16)>) -> TickReport
    where
        S: TerminalShell,
    {
        let mut report = TickReport::default();
        let mut exits = Vec::new();
        let front = self.active;
        for i in &mut self.instances {
            // An exited one is a still picture: nothing to pump, and its exit
            // was already counted.
            if i.exited.is_some() {
                continue;
            }
            let shell = &mut i.shell;
            if let Some(size) = size {
                shell.resize(size);
            }
            // Pumped BEFORE asking for the exit, so the last screen holds
            // the last thing the shell said.
            if shell.pump() {
                if Some(i.id) == front {
                    report.active_output = true;
                } else if !i.unseen {
                    i.unseen = true;
                    report.list_changed = true;
                }
            }
            if let Some(title) = shell.take_title() {
                let title = (!title.is_empty()).then_some(title);
                if title != i.title {
                    i.title = title;
                    report.list_changed = true;
                }
            }
            if let Some(code) = shell.exit_code() {
                exits.push((i.id, code));
            }
        }
        for (id, code) in exits {
            report.list_changed = true;
            if Some(id) == front {
                report.active_output = true;
            }
            self.on_exit(id, code);
        }
        report
    }

    fn get(&self, id: InstanceId) -> Option<&Instance<S>> {
        self.instances.iter().find(|i| i.id == id)
    }

    fn get_mut(&mut self, id: InstanceId) -> Option<&mut Instance<S>> {
        self.instances.iter_mut().find(|i| i.id == id)
    }

    fn position(&self, id: InstanceId) -> Option<usize> {
        self.instances.iter().position(|i| i.id == id)
    }

    /// Moves the front `by` places to the right, wrapping.
    fn step(&mut self, by: usize) {
        let n = self.instances.len();
        let Some(pos) = self.active.and_then(|id| self.position(id)) else {
            return;
        };
        if let Some(id) = self.instances.get((pos + by) % n.max(1)).map(|i| i.id) {
            self.select(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn three() -> (Terminals<&'static str>, [InstanceId; 3]) {
        let mut t = Terminals::new();
        let a = t.push("fish".into(), None, None, "a");
        let b = t.push("bash".into(), None, None, "b");
        let c = t.push("zsh".into(), None, None, "c");
        (t, [a, b, c])
    }

    #[test]
    fn push_appends_and_activates() {
        let (t, [a, _, c]) = three();
        assert_eq!(t.active_id(), Some(c));
        assert_eq!(t.len(), 3);
        assert_eq!(t.iter().next().map(|i| i.id), Some(a));
    }

    /// VS Code's order: the right neighbour, then the left one.
    #[test]
    fn closing_the_active_one_moves_right_then_left() {
        let (mut t, [a, b, c]) = three();
        t.select(b);
        assert!(t.close(b).is_some());
        assert_eq!(t.active_id(), Some(c), "right neighbour first");
        t.close(c);
        assert_eq!(t.active_id(), Some(a), "then left");
        t.close(a);
        assert_eq!(t.active_id(), None);
        assert!(t.is_empty());
    }

    /// The one that comes forward after a close is being SEEN: its mark
    /// goes, or the list keeps announcing output the reader is looking at.
    #[test]
    fn the_one_that_comes_forward_loses_its_unseen_mark() {
        let (mut t, [_, b, c]) = three();
        t.on_output(b);
        t.close(c);
        assert_eq!(t.active_id(), Some(b));
        assert!(!t.active().expect("b").unseen);
    }

    /// A name comes from the renderer: no control or bidi characters, and
    /// capped like a title.
    #[test]
    fn a_name_is_sanitised_and_capped() {
        let (mut t, [a, ..]) = three();
        t.rename(a, "a\u{202e}b\u{1b}c");
        assert_eq!(t.display_title(a), Some("abc"));
        t.rename(a, "a\u{61c}b\u{2028}c\u{2029}d");
        assert_eq!(t.display_title(a), Some("abcd"));
        t.rename(a, &"x".repeat(1000));
        assert_eq!(t.display_title(a).map(|s| s.chars().count()), Some(128));
    }

    #[test]
    fn drain_hands_every_instance_out() {
        let (mut t, _) = three();
        assert_eq!(t.drain().len(), 3);
        assert!(t.is_empty());
        assert_eq!(t.active_id(), None);
    }

    #[test]
    fn closing_an_inactive_one_keeps_the_active() {
        let (mut t, [a, _, c]) = three();
        t.close(a);
        assert_eq!(t.active_id(), Some(c));
    }

    #[test]
    fn closing_an_unknown_id_does_nothing() {
        let (mut t, [_, _, c]) = three();
        assert!(t.close(InstanceId(999)).is_none());
        assert_eq!(t.len(), 3);
        assert_eq!(t.active_id(), Some(c));
    }

    #[test]
    fn next_and_prev_wrap() {
        let (mut t, [a, _, c]) = three();
        t.next();
        assert_eq!(t.active_id(), Some(a));
        t.prev();
        assert_eq!(t.active_id(), Some(c));
    }

    #[test]
    fn exit_zero_removes_and_non_zero_stays_with_its_last_screen() {
        let (mut t, [a, b, _]) = three();
        t.on_exit(a, 0);
        assert!(t.iter().all(|i| i.id != a));
        t.on_exit(b, 3);
        let i = t.iter().find(|i| i.id == b).expect("stays");
        assert_eq!(i.exited, Some(3));
        assert_eq!(i.shell, "b", "the dead shell keeps the last screen");
    }

    /// A clean exit of the ACTIVE one moves the focus like a close does.
    #[test]
    fn exit_zero_of_the_active_moves_to_a_neighbour() {
        let (mut t, [_, b, c]) = three();
        t.on_exit(c, 0);
        assert_eq!(t.active_id(), Some(b));
    }

    #[test]
    fn unseen_only_off_screen_and_select_clears_it() {
        let (mut t, [a, _, c]) = three();
        t.on_output(c);
        t.on_output(a);
        assert!(!t.active().expect("c").unseen, "the active one is seen");
        assert!(t.iter().find(|i| i.id == a).expect("a").unseen);
        t.select(a);
        assert!(!t.active().expect("a").unseen);
    }

    #[test]
    fn title_precedence_name_then_osc_then_profile() {
        let (mut t, [a, ..]) = three();
        assert_eq!(t.display_title(a), Some("fish"));
        t.set_title(a, "vim".into());
        assert_eq!(t.display_title(a), Some("vim"));
        t.rename(a, " build ");
        assert_eq!(t.display_title(a), Some("build"));
        t.rename(a, "   ");
        assert_eq!(t.display_title(a), Some("vim"), "blank name clears it");
    }

    /// An empty OSC title is a program clearing it: back to the profile.
    #[test]
    fn an_empty_osc_title_falls_back_to_the_profile() {
        let (mut t, [a, ..]) = three();
        t.set_title(a, "vim".into());
        t.set_title(a, String::new());
        assert_eq!(t.display_title(a), Some("fish"));
    }

    #[test]
    fn ids_are_never_reused() {
        let (mut t, [_, _, c]) = three();
        t.close(c);
        let d = t.push("x".into(), None, None, "d");
        assert_ne!(d, c);
    }

    #[test]
    fn decorate_sets_and_clears() {
        let (mut t, [a, ..]) = three();
        t.decorate(a, Some(TerminalIcon::Server), AnsiColor::new(2));
        let i = t.iter().find(|i| i.id == a).expect("a");
        assert_eq!(
            (i.icon, i.color.map(AnsiColor::index)),
            (Some(TerminalIcon::Server), Some(2))
        );
        t.decorate(a, None, None);
        let i = t.iter().find(|i| i.id == a).expect("a");
        assert_eq!((i.icon, i.color), (None, None));
    }

    #[test]
    fn clear_drops_everything() {
        let (mut t, _) = three();
        t.clear();
        assert!(t.is_empty());
        assert_eq!(t.active_id(), None);
    }

    /// A shell that says what the test tells it to.
    #[derive(Debug, Default)]
    struct Fake {
        output: bool,
        title: Option<String>,
        code: Option<i32>,
        size: Option<(u16, u16)>,
        pumped: u32,
    }

    impl TerminalShell for Fake {
        fn pump(&mut self) -> bool {
            self.pumped += 1;
            std::mem::take(&mut self.output)
        }
        fn resize(&mut self, size: (u16, u16)) {
            self.size = Some(size);
        }
        fn take_title(&mut self) -> Option<String> {
            self.title.take()
        }
        fn exit_code(&mut self) -> Option<i32> {
            self.code
        }
    }

    fn fakes() -> (Terminals<Fake>, [InstanceId; 2]) {
        let mut t = Terminals::new();
        let a = t.push("sh".into(), None, None, Fake::default());
        let b = t.push("sh".into(), None, None, Fake::default());
        (t, [a, b])
    }

    fn shell(t: &mut Terminals<Fake>, id: InstanceId) -> &mut Fake {
        &mut t
            .iter_mut()
            .find(|i| i.id == id)
            .expect("an instance")
            .shell
    }

    /// A quiet tick reports nothing: the renderer is not woken.
    #[test]
    fn a_quiet_tick_reports_nothing() {
        let (mut t, _) = fakes();
        assert_eq!(t.tick(Some((80, 24))), TickReport::default());
    }

    /// EVERY shell is pumped and resized, not only the one in front: the
    /// pty's buffer keeps only a tail, so an unpumped shell's screen would
    /// come back corrupted.
    #[test]
    fn every_shell_is_pumped_and_resized() {
        let (mut t, [a, b]) = fakes();
        let _ = t.tick(Some((100, 30)));
        for id in [a, b] {
            let s = shell(&mut t, id);
            assert_eq!((s.pumped, s.size), (1, Some((100, 30))));
        }
        let _ = t.tick(None);
        assert_eq!(
            shell(&mut t, a).size,
            Some((100, 30)),
            "None does not resize"
        );
    }

    /// Output behind: marked unseen, the list changes, the front does not.
    #[test]
    fn output_behind_marks_unseen_once() {
        let (mut t, [a, _]) = fakes();
        shell(&mut t, a).output = true;
        assert_eq!(
            t.tick(None),
            TickReport {
                active_output: false,
                list_changed: true
            }
        );
        shell(&mut t, a).output = true;
        assert_eq!(
            t.tick(None),
            TickReport::default(),
            "already unseen: nothing new"
        );
    }

    #[test]
    fn output_in_front_repaints() {
        let (mut t, [_, b]) = fakes();
        shell(&mut t, b).output = true;
        assert!(t.tick(None).active_output);
    }

    /// A title only changes the list if it is a different one.
    #[test]
    fn a_new_title_changes_the_list() {
        let (mut t, [a, _]) = fakes();
        shell(&mut t, a).title = Some("vim".into());
        assert!(t.tick(None).list_changed);
        assert_eq!(t.display_title(a), Some("vim"));
        shell(&mut t, a).title = Some("vim".into());
        assert!(!t.tick(None).list_changed, "same title");
    }

    /// The front exits 0: removed, the neighbour comes forward and must be
    /// painted.
    #[test]
    fn the_front_exiting_cleanly_repaints_the_neighbour() {
        let (mut t, [a, b]) = fakes();
        shell(&mut t, b).code = Some(0);
        let r = t.tick(None);
        assert!(r.list_changed && r.active_output);
        assert_eq!(t.active_id(), Some(a));
    }

    /// The front exits 3: it stays, and it is repainted to say so.
    #[test]
    fn the_front_failing_stays_and_repaints() {
        let (mut t, [_, b]) = fakes();
        shell(&mut t, b).code = Some(3);
        let r = t.tick(None);
        assert!(r.list_changed && r.active_output);
        assert_eq!(t.active().map(|i| i.exited), Some(Some(3)));
        assert_eq!(
            t.tick(None),
            TickReport::default(),
            "an exited one is not pumped"
        );
    }

    #[test]
    fn colour_is_one_to_six_and_icons_round_trip() {
        assert!(AnsiColor::new(0).is_none() && AnsiColor::new(7).is_none());
        assert_eq!(AnsiColor::new(4).map(AnsiColor::index), Some(4));
        for i in TerminalIcon::ALL {
            assert_eq!(TerminalIcon::parse(i.as_str()), Some(i));
        }
        assert_eq!(TerminalIcon::parse("rocket"), None);
    }
}
