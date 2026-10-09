//! The terminal panel's instances: several shells in ONE slot, VS Code
//! style (spec `2026-10-09-terminal-instances-design.md`).
//!
//! Pure: generic over the shell, so the rules are tested without a pty and
//! both frontends obey the same ones (ADR 0077).

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
    /// The shell; `None` once it exited.
    pub shell: Option<S>,
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
            shell: Some(shell),
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
            self.active = self
                .instances
                .get(pos)
                .or_else(|| pos.checked_sub(1).and_then(|p| self.instances.get(p)))
                .map(|i| i.id);
        }
        Some(gone)
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

    /// Names it; a name blank after trimming clears the name.
    pub fn rename(&mut self, id: InstanceId, name: &str) {
        if let Some(i) = self.get_mut(id) {
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
            i.exited = Some(code);
            i.shell = None;
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
    fn exit_zero_removes_and_non_zero_stays_without_shell() {
        let (mut t, [a, b, _]) = three();
        t.on_exit(a, 0);
        assert!(t.iter().all(|i| i.id != a));
        t.on_exit(b, 3);
        let i = t.iter().find(|i| i.id == b).expect("stays");
        assert_eq!(i.exited, Some(3));
        assert!(i.shell.is_none());
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
