//! The places sidebar: drives and favorites, in a panel that stays.
//!
//! PURE state, with no `Backend` and no render: it is handed the volumes
//! `host.volumes` answered and the favorites the config already carries,
//! and it returns rows, cursor and target. The same thing [`crate::help`]
//! does with help, and for the same reason: this way it is tested whole,
//! with no daemon and no terminal.
//!
//! # What this module does NOT decide
//!
//! WHEN the volumes are requested. A sidebar that polled would be ADR
//! 0058's suspension rule broken from the first frame, so whoever paints
//! it requests them on opening and on refresh, and never by clock.
//!
//! # Two sections, not three
//!
//! There is no "Remote": norte has no connection list yet (#140), and an
//! `sftp://` saved as a favorite already shows up under Favorites.
//! Inventing the section with no source behind it would be an empty box
//! promising something.

use norte_proto::VPath;
use norte_proto::methods::{Volume, VolumeKind};

/// A drive's SHORT name: its label if it has one, and if not, the last
/// segment of its mount point (the root is said whole). With the masking
/// flag, like every paint gateway.
///
/// The whole mount point cut off — "/home/oscar/…" five times in the
/// 2026-09-21 capture's places bar — did not distinguish one drive from
/// another; its last segment did. The whole path is still available where
/// there is room for it (the row's title in the window).
///
/// ```
/// use norte_frontend::places::drive_name;
/// use norte_proto::VPath;
/// let vp = |w: &str| VPath::parse(w).unwrap();
/// assert_eq!(drive_name(b"", &vp("file:///home/ana/nube")).0, "nube");
/// assert_eq!(drive_name(b"USB", &vp("file:///media/x")).0, "USB");
/// assert!(drive_name(b"", &vp("file:///")).0.ends_with('/'));
/// ```
#[must_use]
pub fn drive_name(label: &[u8], mount: &VPath) -> (String, bool) {
    if !label.is_empty() {
        return crate::display_name(label);
    }
    match mount.file_name() {
        Some(seg) => crate::display_name(seg.as_bytes()),
        None => crate::path_display(mount),
    }
}

/// The sidebar's sections, in the order they are painted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    /// The host's volumes, with their space.
    Drives,
    /// The user's hotlist.
    Favorites,
}

impl Section {
    /// Its header's Fluent key.
    ///
    /// ```
    /// use norte_frontend::places::Section;
    /// assert_eq!(Section::Drives.label_key(), "places-section-drives");
    /// ```
    #[must_use]
    pub const fn label_key(self) -> &'static str {
        match self {
            Self::Drives => "places-section-drives",
            Self::Favorites => "places-section-favorites",
        }
    }
}

/// A paintable sidebar row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaceRow {
    /// A section's header. Does not navigate.
    Header {
        /// Which section.
        section: Section,
        /// Is it folded?
        folded: bool,
    },
    /// A host volume.
    Drive {
        /// What the system calls it, in BYTES: no platform promises a
        /// volume's label is UTF-8 (rule 1). Empty = no label, and then
        /// the mount point is painted.
        label: Vec<u8>,
        /// Where it is mounted.
        mount: VPath,
        /// Free space, or `None` if the filesystem did not answer.
        ///
        /// `None` is NOT zero: a zero here would read as "full". It is the
        /// same rule [`crate::space`] applies to the notice before a copy.
        free: Option<u64>,
        /// Total space, with the same warning as [`Self::Drive::free`].
        total: Option<u64>,
        /// Is it mounted read-only?
        read_only: bool,
        /// What kind of drive it is (fixed, removable, network): what
        /// decides its icon in the window.
        kind: VolumeKind,
    },
    /// A hotlist favorite.
    Favorite {
        /// The name the user gave it.
        name: String,
        /// Its target, or the Fluent key for the error if the path does
        /// not parse.
        ///
        /// A broken favorite IS PAINTED, with its reason: one that
        /// disappears silently is a config failure nobody can see.
        target: Result<VPath, String>,
    },
}

/// The whole sidebar: its two sources, what is folded and where the cursor
/// is.
///
/// ```
/// use norte_frontend::places::{PlaceRow, PlacesState};
/// use norte_proto::VPath;
///
/// let mut s = PlacesState::new();
/// s.set_favorites(&[(
///     "casa".to_owned(),
///     Ok(VPath::parse("file:///home").expect("wire")),
/// )]);
/// // BOTH headers are always there, even if a section is empty: with no
/// // volumes yet, the list does not jump when they arrive. The cursor
/// // starts on the first header, which navigates nowhere.
/// assert_eq!(s.rows().len(), 3);
/// assert!(s.activate().is_none());
/// s.down();
/// s.down();
/// assert!(matches!(s.rows()[s.cursor()], PlaceRow::Favorite { .. }));
/// assert!(s.activate().is_some());
/// ```
#[derive(Debug, Clone)]
pub struct PlacesState {
    drives: Vec<PlaceRow>,
    favorites: Vec<PlaceRow>,
    drives_folded: bool,
    favorites_folded: bool,
    rows: Vec<PlaceRow>,
    cursor: usize,
}

impl Default for PlacesState {
    fn default() -> Self {
        Self::new()
    }
}

impl PlacesState {
    /// An empty sidebar: no volumes and no favorites yet.
    ///
    /// Empty of CONTENT, not of rows: both headers exist from the first
    /// frame. Without them, a freshly opened panel would be a blank box
    /// while `host.volumes` answers, and folding would mean nothing
    /// because the cursor would not be in any section.
    #[must_use]
    pub fn new() -> Self {
        let mut s = Self {
            drives: Vec::new(),
            favorites: Vec::new(),
            drives_folded: false,
            favorites_folded: false,
            rows: Vec::new(),
            cursor: 0,
        };
        s.rebuild();
        s
    }

    /// What is said about a volume's SPACE, and whether it is read-only.
    ///
    /// One single function because there used to be three — two of them in
    /// the same crate — and they already differed in how they write the
    /// numbers. Worse: with `free` known and `total` unknown, all three
    /// said "unknown", throwing away the one piece of data there was. And
    /// how much is LEFT is exactly the half looked at before copying; how
    /// big the disk is, nobody looks at.
    ///
    /// A size the system did not answer IS SAID, and never replaced with a
    /// zero: a zero reads as "full", which is the opposite of "I don't
    /// know".
    ///
    /// `corto` picks the numbers' scale, and that difference is real: the
    /// side bar is half the width of a full-screen picker.
    ///
    /// ```
    /// use norte_frontend::places::PlacesState;
    /// use norte_i18n::Lang;
    ///
    /// // The normal case: both numbers.
    /// let d = PlacesState::volume_detail(Some(1_000), Some(4_000), false, false, Lang::En);
    /// assert!(d.contains("free of"));
    /// // Only what is left: what is known is said, instead of "unknown".
    /// let medio = PlacesState::volume_detail(Some(1_000), None, false, false, Lang::En);
    /// assert!(medio.contains("free") && !medio.contains("unknown"));
    /// // Nothing: then yes.
    /// assert!(PlacesState::volume_detail(None, None, false, false, Lang::En).contains("unknown"));
    /// // And read-only is added, not substituted.
    /// let ro = PlacesState::volume_detail(None, None, true, false, Lang::En);
    /// assert!(ro.contains("read-only") && ro.contains("unknown"));
    /// ```
    #[must_use]
    pub fn volume_detail(
        free: Option<u64>,
        total: Option<u64>,
        read_only: bool,
        corto: bool,
        lang: norte_i18n::Lang,
    ) -> String {
        let bytes = |n: u64| {
            if corto {
                crate::human_bytes_short(n)
            } else {
                crate::human_bytes(n)
            }
        };
        let mut parts = Vec::new();
        match (free, total) {
            (Some(f), Some(t)) => parts.push(norte_i18n::ta_in(
                lang,
                "picker-volume-space",
                &[("free", &bytes(f)), ("total", &bytes(t))],
            )),
            // What is known, even if it is only half.
            (Some(f), None) => parts.push(norte_i18n::ta_in(
                lang,
                "picker-volume-free",
                &[("free", &bytes(f))],
            )),
            // With only the total, there is nothing useful to say: how big
            // the disk is changes no decision.
            _ => parts.push(norte_i18n::t_in(lang, "volumes-size-unknown")),
        }
        if read_only {
            parts.push(norte_i18n::t_in(lang, "picker-volume-read-only"));
        }
        parts.join(" · ")
    }

    /// Replaces the volumes with the ones the host just answered.
    ///
    /// Replaces, does not merge: the mount list is a SNAPSHOT, and keeping
    /// one that is no longer there would be offering a place you cannot go
    /// to.
    pub fn set_drives(&mut self, volumes: &[Volume]) {
        self.drives = volumes
            .iter()
            .map(|v| PlaceRow::Drive {
                label: v.label.clone().unwrap_or_default(),
                mount: v.mount.clone(),
                free: v.free_bytes,
                total: v.total_bytes,
                read_only: v.read_only,
                kind: v.kind,
            })
            .collect();
        self.rebuild();
    }

    /// Replaces the favorites.
    ///
    /// Receives the pair already broken down and not the config's type:
    /// this crate has no reason to depend on `norte-config` for a
    /// two-field struct, and the frontend that has it in front translates
    /// it at the call site.
    pub fn set_favorites(&mut self, items: &[(String, Result<VPath, String>)]) {
        self.favorites = items
            .iter()
            .map(|(name, target)| PlaceRow::Favorite {
                name: name.clone(),
                target: target.clone(),
            })
            .collect();
        self.rebuild();
    }

    /// The VISIBLE rows, headers included and without what is folded.
    #[must_use]
    pub fn rows(&self) -> &[PlaceRow] {
        &self.rows
    }

    /// Where the cursor is within [`Self::rows`].
    #[must_use]
    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    /// Puts the cursor on row `i`, clamped to what there is.
    ///
    /// Requested by the MOUSE (#226): a click names a row by its POSITION,
    /// and reaching it by `up`/`down` would mean reimplementing the
    /// cursor's arithmetic in the frontend. Out of range is clamped
    /// instead of doing nothing: a list that shrank between the frame and
    /// the click must not leave the cursor where it was.
    pub fn set_cursor(&mut self, i: usize) {
        self.cursor = i.min(self.rows.len().saturating_sub(1));
    }

    /// Moves the cursor up one row. Stays on the first one.
    pub const fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Moves the cursor down one row. Stays on the last one.
    pub fn down(&mut self) {
        let last = self.rows.len().saturating_sub(1);
        self.cursor = self.cursor.saturating_add(1).min(last);
    }

    /// Folds or unfolds the section the cursor is on.
    ///
    /// From any row, the section it belongs to is enough, so folding does
    /// not require going up to the header first.
    pub fn toggle_fold(&mut self) {
        match self.section_at(self.cursor) {
            Some(Section::Drives) => self.drives_folded = !self.drives_folded,
            Some(Section::Favorites) => self.favorites_folded = !self.favorites_folded,
            None => return,
        }
        self.rebuild();
    }

    /// Where the cursor's row leads, or `None`.
    ///
    /// `None` on a header and on a broken favorite: both are painted, and
    /// neither is a place.
    #[must_use]
    pub fn activate(&self) -> Option<&VPath> {
        match self.rows.get(self.cursor)? {
            PlaceRow::Header { .. } => None,
            PlaceRow::Drive { mount, .. } => Some(mount),
            PlaceRow::Favorite { target, .. } => target.as_ref().ok(),
        }
    }

    /// Is that section folded?
    ///
    /// Asked by whoever has the `Backend` in front: unfolding the drives
    /// is the moment to request them again, and folding them is the
    /// moment NOT to.
    #[must_use]
    pub const fn is_folded(&self, section: Section) -> bool {
        match section {
            Section::Drives => self.drives_folded,
            Section::Favorites => self.favorites_folded,
        }
    }

    /// Which section row `i` belongs to.
    fn section_at(&self, i: usize) -> Option<Section> {
        let mut current = None;
        for (j, row) in self.rows.iter().enumerate() {
            if let PlaceRow::Header { section, .. } = row {
                current = Some(*section);
            }
            if j == i {
                return current;
            }
        }
        None
    }

    /// Rebuilds the visible rows and repositions the cursor within them.
    ///
    /// The second part is the half that gets forgotten: folding a section
    /// with the cursor inside would leave it pointing at a row that no
    /// longer exists.
    fn rebuild(&mut self) {
        let mut out = Vec::with_capacity(self.rows.len() + 2);
        out.push(PlaceRow::Header {
            section: Section::Drives,
            folded: self.drives_folded,
        });
        if !self.drives_folded {
            out.extend(self.drives.iter().cloned());
        }
        out.push(PlaceRow::Header {
            section: Section::Favorites,
            folded: self.favorites_folded,
        });
        if !self.favorites_folded {
            out.extend(self.favorites.iter().cloned());
        }
        self.rows = out;
        self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
    }
}

/// The label a directory is PRESENTED with: its sanitized last segment,
/// and at the root the host (or the slash, if the scheme names none).
fn dir_label(dir: &VPath) -> String {
    dir.file_name().map_or_else(
        || {
            dir.authority()
                .map_or_else(|| "/".to_owned(), |a| crate::display_name(a.as_bytes()).0)
        },
        |seg| crate::display_name(seg.as_bytes()).0,
    )
}

/// The name PROPOSED for a new favorite over `dir`, already clear of the
/// `taken` ones the hotlist has set.
///
/// A favorite's name is a LABEL, not a path — the target travels
/// separately, in `path` — so it is suggested SANITIZED
/// ([`crate::display_name`]): a non-UTF-8 name or one with terminal hazards
/// does not go raw into the user's `norte.toml`.
///
/// # Why it dodges names already taken
///
/// `persist_hotlist_add` REPLACES the entry whose name already exists.
/// With the field pre-filled, the reflex of accepting without reading
/// would silently overwrite a favorite that pointed somewhere else, and
/// `src` or `docs` collide constantly. So the suggestion is qualified with
/// the parent directory — which also says more than a number — and only
/// numbers when even that is not enough. A TYPED name that collides still
/// replaces: that is what the human asked for.
///
/// ```
/// use norte_frontend::places::suggested_hotlist_name;
/// use norte_proto::VPath;
///
/// let dir = VPath::parse("file:///home/o/norte/src").expect("wire");
/// assert_eq!(suggested_hotlist_name(&dir, &[]), "src");
/// assert_eq!(suggested_hotlist_name(&dir, &["src"]), "norte/src");
/// ```
#[must_use]
pub fn suggested_hotlist_name(dir: &VPath, taken: &[&str]) -> String {
    let base = dir_label(dir);
    if !taken.contains(&base.as_str()) {
        return base;
    }
    if let Some(parent) = dir.parent().and_then(|p| p.file_name().cloned()) {
        let qualified = format!("{}/{base}", crate::display_name(parent.as_bytes()).0);
        if !taken.contains(&qualified.as_str()) {
            return qualified;
        }
    }
    // Terminates: `taken` is finite, so some `n` is free.
    let mut n = 2u32;
    loop {
        let candidate = format!("{base} ({n})");
        if !taken.contains(&candidate.as_str()) {
            return candidate;
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::methods::{Volume, VolumeKind};

    fn vp(wire: &str) -> VPath {
        VPath::parse(wire).expect("valid wire")
    }

    fn volumen(mount: &str, free: Option<u64>, total: Option<u64>) -> Volume {
        Volume {
            mount: vp(mount),
            label: None,
            fs_type: "ext4".to_owned(),
            kind: VolumeKind::Fixed,
            total_bytes: total,
            free_bytes: free,
            read_only: false,
        }
    }

    fn con_label(label: Vec<u8>) -> Volume {
        Volume {
            label: Some(label),
            ..volumen("file:///", Some(1), Some(2))
        }
    }

    /// A broken favorite IS PAINTED, with its reason. One that disappears
    /// silently is a config failure you cannot see.
    #[test]
    fn un_favorito_roto_sale_en_la_lista_y_no_navega() {
        let mut s = PlacesState::new();
        s.set_favorites(&[
            ("bueno".to_owned(), Ok(vp("file:///casa"))),
            ("roto".to_owned(), Err("err-invalid-path".to_owned())),
        ]);
        // Drives header (empty), favorites header, and both.
        assert_eq!(s.rows().len(), 4);
        assert!(matches!(
            s.rows()[1],
            PlaceRow::Header {
                section: Section::Favorites,
                ..
            }
        ));
        s.down();
        s.down();
        s.down();
        assert!(matches!(s.rows()[s.cursor()], PlaceRow::Favorite { .. }));
        assert!(s.activate().is_none(), "the broken one leads nowhere");
    }

    /// A missing `free_bytes` is NOT zero: it is "did not answer". The rule
    /// lives in `space.rs` and here the `Option` is kept as is, without
    /// substituting it with a number that would read as "full".
    #[test]
    fn un_volumen_sin_espacio_conserva_el_none() {
        let mut s = PlacesState::new();
        s.set_drives(&[volumen("file:///mnt", None, None)]);
        let PlaceRow::Drive { free, total, .. } = &s.rows()[1] else {
            panic!("row 1 is the volume");
        };
        assert!(free.is_none() && total.is_none());
    }

    /// Folding hides the section's rows and leaves the cursor within what
    /// remains.
    #[test]
    fn plegar_una_seccion_esconde_sus_filas_y_recoloca_el_cursor() {
        let mut s = PlacesState::new();
        s.set_drives(&[volumen("file:///", Some(1000), Some(4000))]);
        s.set_favorites(&[("casa".to_owned(), Ok(vp("file:///casa")))]);
        assert_eq!(s.rows().len(), 4);
        // Cursor all the way at the end, which is where folding hurts.
        for _ in 0..10 {
            s.down();
        }
        assert_eq!(s.cursor(), 3);
        s.toggle_fold();
        assert_eq!(s.rows().len(), 3);
        assert!(s.cursor() < s.rows().len());
    }

    /// `is_folded` says the same thing the header paints: it is what
    /// whoever decides whether to request the volumes again looks at.
    #[test]
    fn is_folded_sigue_al_toggle() {
        let mut s = PlacesState::new();
        assert!(!s.is_folded(Section::Drives));
        s.toggle_fold();
        assert!(s.is_folded(Section::Drives));
        assert!(!s.is_folded(Section::Favorites));
    }

    /// Folding from any row folds ITS section, not the first one.
    #[test]
    fn plegar_desde_una_fila_pliega_su_propia_seccion() {
        let mut s = PlacesState::new();
        s.set_drives(&[volumen("file:///", Some(1), Some(2))]);
        s.set_favorites(&[("casa".to_owned(), Ok(vp("file:///casa")))]);
        s.down(); // on the volume
        s.toggle_fold();
        assert!(matches!(
            s.rows()[0],
            PlaceRow::Header {
                section: Section::Drives,
                folded: true
            }
        ));
        assert!(matches!(
            s.rows()[1],
            PlaceRow::Header {
                section: Section::Favorites,
                folded: false
            }
        ));
    }

    /// A volume's label is BYTES (rule 1): a name that is not UTF-8 does
    /// not blow up or get lost along the way.
    #[test]
    fn una_etiqueta_no_utf8_sobrevive_como_bytes() {
        let mut s = PlacesState::new();
        s.set_drives(&[con_label(b"\xffdisco".to_vec())]);
        let PlaceRow::Drive { label, .. } = &s.rows()[1] else {
            panic!("volume")
        };
        assert_eq!(label, b"\xffdisco");
    }

    /// A volume that disappears from the host disappears from the list:
    /// offering a mount that is no longer there is offering a place you
    /// cannot go to.
    #[test]
    fn set_drives_sustituye_no_fusiona() {
        let mut s = PlacesState::new();
        s.set_drives(&[volumen("file:///", Some(1), Some(2))]);
        s.set_drives(&[volumen("file:///mnt", Some(1), Some(2))]);
        assert_eq!(s.rows().len(), 3);
        let PlaceRow::Drive { mount, .. } = &s.rows()[1] else {
            panic!("volume")
        };
        assert_eq!(*mount, vp("file:///mnt"));
    }

    #[test]
    fn la_sugerencia_es_el_ultimo_segmento() {
        assert_eq!(
            suggested_hotlist_name(&vp("file:///home/o/work"), &[]),
            "work"
        );
    }

    /// The local root has no last segment, and `file` names no place: the
    /// name a human recognizes there is the slash.
    #[test]
    fn la_raiz_local_se_sugiere_como_barra() {
        assert_eq!(suggested_hotlist_name(&vp("file:///"), &[]), "/");
    }

    /// At a remote's root there IS something naming the place: the host.
    #[test]
    fn la_raiz_remota_se_sugiere_con_su_authority() {
        assert_eq!(suggested_hotlist_name(&vp("sftp://host/"), &[]), "host");
    }

    /// The name is a LABEL (the target travels separately, in `path`), so
    /// it is suggested SANITIZED: non-UTF-8 bytes come out lossy and
    /// terminal hazards masked, and none go raw into `norte.toml`.
    #[test]
    fn la_sugerencia_va_saneada_como_cualquier_nombre_pintado() {
        assert_eq!(
            suggested_hotlist_name(&vp("file:///home/%FFdir"), &[]),
            "\u{FFFD}dir"
        );
        assert_eq!(
            suggested_hotlist_name(&vp("file:///home/%E2%80%AEdir"), &[]),
            "\u{FFFD}dir"
        );
    }

    /// `persist_hotlist_add` REPLACES if the name already exists: a
    /// colliding suggestion turns the `a`+Enter reflex into overwriting a
    /// favorite that pointed somewhere else. The suggestion is qualified
    /// with the parent, which also says more than a number.
    #[test]
    fn una_sugerencia_ocupada_se_cualifica_con_el_padre() {
        assert_eq!(
            suggested_hotlist_name(&vp("file:///home/o/norte/src"), &["src"]),
            "norte/src"
        );
    }

    /// If the parent is not enough either, it numbers. And the number goes
    /// up until it finds room: stopping at the first taken one would
    /// collide again.
    #[test]
    fn si_el_padre_tampoco_basta_se_numera_hasta_encontrar_hueco() {
        assert_eq!(
            suggested_hotlist_name(&vp("file:///home/o/norte/src"), &["src", "norte/src"]),
            "src (2)"
        );
        assert_eq!(
            suggested_hotlist_name(
                &vp("file:///home/o/norte/src"),
                &["src", "norte/src", "src (2)"]
            ),
            "src (3)"
        );
    }

    /// With no parent to qualify with (the root), it numbers directly.
    #[test]
    fn la_raiz_ocupada_se_numera_sin_padre() {
        assert_eq!(suggested_hotlist_name(&vp("file:///"), &["/"]), "/ (2)");
    }
}
