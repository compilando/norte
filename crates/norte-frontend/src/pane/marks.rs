//! Las marcas: qué está elegido para operar, y el barrido que las pinta con
//! el ratón.
//!
//! Es la mitad del pane que decide SOBRE QUÉ actúa una operación, así que
//! vive junta: marcar una a una, todas, por patrón, invertir, y el barrido
//! —que es una edición TENTATIVA con su línea base, para que soltar el botón
//! donde empezaste no deje media selección hecha.

use super::{
    Entry, EntryKind, GlobBuilder, HashSet, Mode, PaneState, PatternError, VPath,
    unicode_glob_regex,
};

impl PaneState {
    /// Togglea la marca de la entrada seleccionada (respeta el filtro quick:
    /// marca la entrada VISIBLE bajo la selección). No-op si no hay selección.
    pub fn toggle_mark(&mut self) {
        let Some(path) = self.selected().map(|e| e.path.clone()) else {
            return;
        };
        if !self.marks.remove(&path) {
            self.marks.insert(path);
        }
    }

    /// mc/Total Commander sweep (`insert`, #103): toggle-mark the VISIBLE
    /// selection, then advance to the next visible row — holding the key
    /// selects a range. With a [`Mode::Filter`] quick search active, "next"
    /// means the next VISIBLE row within the filter ([`Self::quick_down`],
    /// which does not wrap); the real cursor is left untouched, exactly as
    /// [`Self::toggle_mark`] itself only ever acts on the filtered
    /// selection. Without an active filter (or in [`Mode::Jump`], where
    /// [`Self::selected`] already reads the real cursor), it advances the
    /// real cursor ([`Self::page_down`], which clamps). Either way, at the
    /// last visible row this marks WITHOUT wrapping back to the top.
    pub fn toggle_mark_and_advance(&mut self) {
        self.toggle_mark();
        let filtering = self
            .quick
            .as_ref()
            .is_some_and(|q| q.mode() == Mode::Filter);
        if filtering {
            self.quick_down();
        } else {
            self.page_down(1);
        }
    }

    /// ¿Está marcada esta entrada? (por su `VPath` absoluto).
    #[must_use]
    pub fn is_marked(&self, entry: &Entry) -> bool {
        self.marks.contains(&entry.path)
    }

    /// Cuántas entradas marcadas.
    #[must_use]
    pub fn marks_len(&self) -> usize {
        self.marks.len()
    }

    /// Los `VPath` sobre los que opera la acción: las marcas (en el ORDEN de
    /// `entries`, determinista), o la selección (respeta el filtro quick) si
    /// no hay marcas (vacío si tampoco hay selección). Fuente única de "sobre
    /// qué opera la op".
    #[must_use]
    pub fn marked_paths(&self) -> Vec<VPath> {
        if self.marks.is_empty() {
            return self
                .selected()
                .map(|e| e.path.clone())
                .into_iter()
                .collect();
        }
        self.entries
            .iter()
            .filter(|e| self.marks.contains(&e.path))
            .map(|e| e.path.clone())
            .collect()
    }

    /// Limpia todas las marcas.
    pub fn clear_marks(&mut self) {
        self.marks.clear();
    }

    /// Vuelve a marcar, POR RUTA, lo que siga estando en el listado.
    ///
    /// Existe para el REFRESCO: `set_listing` limpia las marcas porque las
    /// filas son otras y una marca por índice apuntaría a otro fichero. Eso
    /// es correcto para un `cd`, y castiga a quien no se movió — un listado
    /// que se recarga solo (una copia que termina, un vigilante) se llevaba
    /// por delante una selección que el lector había hecho a mano.
    ///
    /// La identidad es el `VPath` BYTE A BYTE, como en todo el resto: una
    /// entrada que ya no está —la acaba de borrar la operación— simplemente
    /// no se vuelve a marcar, y no se inventa nada. Lo que devuelve es
    /// cuántas se perdieron, porque una selección que encoge sin decirlo es
    /// una operación posterior sobre menos ficheros de los que el lector
    /// cree.
    ///
    /// ```
    /// use norte_frontend::PaneState;
    /// use norte_proto::{Entry, EntryKind, VPath};
    ///
    /// let dir = VPath::parse("mem:///d").unwrap();
    /// fn entrada(dir: &VPath, n: &str) -> Entry {
    ///     Entry {
    ///         path: dir.join(norte_proto::Segment::new(n.as_bytes().to_vec()).unwrap()),
    ///         kind: EntryKind::File,
    ///         size: None,
    ///         mtime_ms: None,
    ///         attrs: Default::default(),
    ///     }
    /// }
    /// let mut p = PaneState::new(
    ///     dir.clone(),
    ///     vec![entrada(&dir, "a"), entrada(&dir, "b")],
    /// );
    /// p.mark_all();
    /// let antes = p.marked_paths();
    /// assert_eq!(antes.len(), 2);
    ///
    /// // El listado se recarga y `b` ya no está.
    /// p.set_listing(dir.clone(), vec![entrada(&dir, "a")]);
    /// assert_eq!(p.marks_len(), 0, "un listado nuevo llega sin marcas");
    /// assert_eq!(p.restore_marks(&antes), 1, "una se perdió, y se dice");
    /// assert_eq!(p.marks_len(), 1);
    /// ```
    pub fn restore_marks(&mut self, paths: &[VPath]) -> usize {
        let mut perdidas = 0;
        for path in paths {
            if self.entries.iter().any(|e| &e.path == path) {
                self.marks.insert(path.clone());
            } else {
                perdidas += 1;
            }
        }
        perdidas
    }

    /// The indices a BULK mark acts on: the VISIBLE subset under an active
    /// quick filter, the whole listing otherwise — what you see is what you
    /// mark. While a fill is running ([`Self::loading`]) it reaches only what
    /// has been drained so far; the pane already marks an in-progress listing
    /// (the title in the TUI, a status line in the GUI), so the partial reach
    /// is never silent.
    pub(super) fn markable_indices(&self) -> Vec<usize> {
        match self.quick_visible() {
            Some(vis) => vis.to_vec(),
            None => (0..self.entries.len()).collect(),
        }
    }

    /// Marks every entry of the visible set (see `markable_indices`).
    pub fn mark_all(&mut self) {
        for i in self.markable_indices() {
            let Some(path) = self.entries.get(i).map(|e| &e.path) else {
                continue;
            };
            if !self.marks.contains(path) {
                self.marks.insert(path.clone());
            }
        }
    }

    /// Marks every entry between two indices of [`Self::entries`],
    /// INCLUSIVE, in either order (`mark_range(7, 2)` is `mark_range(2, 7)`)
    /// — the primitive a shift+click and a pointer sweep need, where the
    /// anchor sits either side of the pointer and the caller should not
    /// have to sort them first. Returns how many marks it ADDED, never the
    /// resulting total, exactly like [`Self::mark_glob`]: a range over
    /// already-marked entries returns 0 while the selection stays
    /// non-empty; read [`Self::marks_len`] for the total.
    ///
    /// It only ever ADDS — this is the ADDITIVE marker, the one a
    /// shift+click wants (it extends a selection built by hand and must not
    /// take anything back). A pointer SWEEP wants the opposite and uses
    /// [`Self::apply_sweep`], which rubber-bands against a baseline.
    /// [`Self::set_mark`] is the only way to clear a mark by index.
    ///
    /// Under an active [`Mode::Filter`] quick search it reaches only the
    /// VISIBLE subset (`markable_indices`, the same rule as
    /// [`Self::mark_all`]): what you cannot see, you cannot mark. A range
    /// whose ends straddle a filtered-out entry leaves that entry alone, so
    /// the next bulk operation never widens onto a file the filter was
    /// hiding.
    ///
    /// The range is CLAMPED to the listing, not rejected: a range that runs
    /// off the end marks up to the last entry, which is exactly what a hit
    /// test in the blank area below the last row produces. A range entirely
    /// outside the listing (and any range on an empty listing) therefore
    /// marks nothing.
    ///
    /// ```
    /// # use norte_frontend::PaneState;
    /// # use norte_proto::{Entry, EntryKind, VPath};
    /// # fn e(w: &str) -> Entry {
    /// #     Entry { attrs: Default::default(), path: VPath::parse(w).unwrap(),
    /// #             kind: EntryKind::File, size: None, mtime_ms: None }
    /// # }
    /// let mut p = PaneState::new(
    ///     VPath::parse("mem:///").unwrap(),
    ///     vec![e("mem:///a"), e("mem:///b"), e("mem:///c")],
    /// );
    /// assert_eq!(p.mark_range(2, 0), 3, "either order, inclusive");
    /// // The return is what CHANGED, never the resulting total: re-marking
    /// // the same range changes nothing while the selection stays full.
    /// assert_eq!(p.mark_range(0, 2), 0);
    /// assert_eq!(p.marks_len(), 3);
    /// ```
    pub fn mark_range(&mut self, from: usize, to: usize) -> usize {
        let (lo, hi) = if from <= to { (from, to) } else { (to, from) };
        // Recorre el RANGO, no todo el listado: un barrido re-enuncia su
        // rango a ritmo de evento de puntero, y `markable_indices` cuesta
        // un `Vec` del tamaño del listado en cada llamada.
        let Some(last) = self.entries.len().checked_sub(1) else {
            return 0;
        };
        if lo > last {
            return 0;
        }
        let hi = hi.min(last);
        let mut changed = 0usize;
        for i in lo..=hi {
            if !self.is_markable(i) {
                continue;
            }
            let Some(entry) = self.entries.get(i) else {
                continue;
            };
            // `contains` antes de clonar: la re-emisión de un barrido pasa
            // por aquí a ritmo de evento de puntero y la inmensa mayoría de
            // las filas del rango ya están marcadas — clonar un `VPath`
            // para que el `HashSet` lo tire era el coste dominante.
            if self.marks.contains(&entry.path) {
                continue;
            }
            let path = entry.path.clone();
            self.marks.insert(path);
            changed += 1;
        }
        changed
    }

    /// Arms a pointer sweep: drops any baseline left by a previous one, so
    /// the next [`Self::apply_sweep`] snapshots the marks as they are NOW.
    /// Cheap (no clone); call it when the gesture starts.
    ///
    /// Without this, a sweep that follows unrelated marking (a ctrl+click,
    /// a `mark_glob`) would restore the previous gesture's baseline and
    /// silently drop everything marked in between.
    pub fn begin_sweep(&mut self) {
        self.sweep_baseline = None;
        self.sweep_extent = None;
    }

    /// Applies the CURRENT extent of a pointer sweep: gives back the rows
    /// the sweep covered a moment ago and no longer does, then marks
    /// `from..=to` through [`Self::mark_range`], so the filter still decides
    /// what is reachable. Returns the marks added on top of what was already
    /// there.
    ///
    /// Both directions are DELTAS over the extent that changed, never a
    /// rebuild: rows that LEFT the range are given back (only those the
    /// sweep itself added — anything in the baseline is untouched) and only
    /// rows that ENTERED it are marked. A one-row motion therefore costs one
    /// row. This matters: at GPUI's per-pixel event rate on a 20 000-entry
    /// pane, restoring a snapshot per motion measured 7.1 ms per event and
    /// re-marking the whole range 2.7 ms, against 0.03 ms for the delta.
    ///
    /// The extent is dropped by any listing change, so the first call after
    /// one re-marks its whole range rather than trusting indices that moved.
    /// A quick filter that changes DURING a gesture is not re-examined: a
    /// row that becomes visible mid-sweep stays unmarked until the pointer
    /// moves over it again. Nothing is lost, and a drag with a hand on the
    /// filter is not a gesture worth a full rescan per motion.
    ///
    /// This is what makes a drag RUBBER-BAND. An add-only sweep leaves
    /// behind everything the pointer ever touched: overshooting by fifteen
    /// rows and pulling back leaves fifteen files marked, and because an
    /// overshoot happens at the viewport edge under autoscroll, those rows
    /// are precisely the ones that just scrolled out of sight. The next
    /// bulk operation would then act on files the user pulled back from and
    /// cannot see — and with no range unmarker, undoing that by hand is one
    /// ctrl+click per surplus row.
    ///
    /// The baseline is a snapshot of the mark SET, so marks made before the
    /// gesture survive every retreat. It is dropped by any listing change
    /// (see the `sweep_baseline` field); after one, the next call
    /// re-snapshots and the sweep simply starts rubber-banding from there.
    pub fn apply_sweep(&mut self, from: usize, to: usize) -> usize {
        let (lo, hi) = if from <= to { (from, to) } else { (to, from) };
        if self.sweep_baseline.is_none() {
            // Foto perezosa: el gesto pudo armarse en un simple click que
            // jamás barre, y clonar el conjunto de marcas en cada click
            // sería un coste que nadie pidió.
            self.sweep_baseline = Some(self.marks.clone());
            self.sweep_extent = None;
        }
        if let (Some(baseline), Some((plo, phi))) = (&self.sweep_baseline, self.sweep_extent) {
            let phi = phi.min(self.entries.len().saturating_sub(1));
            let mut soltar: Vec<VPath> = Vec::new();
            for i in plo..=phi {
                if i >= lo && i <= hi {
                    continue;
                }
                let Some(entry) = self.entries.get(i) else {
                    continue;
                };
                // Solo se suelta lo que puso ESTE barrido: lo que ya estaba
                // marcado antes del gesto está en la baseline y no se toca.
                if !baseline.contains(&entry.path) {
                    soltar.push(entry.path.clone());
                }
            }
            for path in soltar {
                self.marks.remove(&path);
            }
        }
        // Marca solo lo que ENTRA en el rango: el resto del solape ya lo
        // marcó una llamada anterior de este mismo barrido. Dos intervalos
        // como mucho, así que un motion de una fila cuesta una fila y no un
        // repaso del listado entero.
        let previo = self.sweep_extent;
        self.sweep_extent = Some((lo, hi));
        match previo {
            Some((plo, phi)) if lo <= phi && hi >= plo => {
                let mut changed = 0usize;
                if lo < plo {
                    changed += self.mark_range(lo, plo - 1);
                }
                if hi > phi {
                    changed += self.mark_range(phi + 1, hi);
                }
                changed
            }
            _ => self.mark_range(lo, hi),
        }
    }

    /// Gives back EVERYTHING the sweep in progress marked, restoring the
    /// baseline [`Self::apply_sweep`] snapshotted, and keeps the gesture
    /// armed: a later `apply_sweep` starts rubber-banding from the same
    /// baseline, so a pointer that leaves and comes back loses nothing.
    ///
    /// It is [`Self::apply_sweep`] with an EMPTY extent, and it exists for
    /// the moment a mark sweep stops being one: a drag that crosses into the
    /// other pane is promoted to a transfer
    /// ([`crate::mouse::Effect::RevertSweep`]), and a promotion changes what
    /// the gesture DOES, not what is selected — the rows it swept on the way
    /// out must not stay marked behind it.
    ///
    /// Marks made BEFORE the gesture survive (they are in the baseline),
    /// exactly as they survive a retreat. Without an armed sweep it is a
    /// no-op.
    ///
    /// ```
    /// # use norte_frontend::PaneState;
    /// # use norte_proto::{Entry, EntryKind, VPath};
    /// # fn e(w: &str) -> Entry {
    /// #     Entry { attrs: Default::default(), path: VPath::parse(w).unwrap(),
    /// #             kind: EntryKind::File, size: None, mtime_ms: None }
    /// # }
    /// let mut p = PaneState::new(
    ///     VPath::parse("mem:///").unwrap(),
    ///     vec![e("mem:///a"), e("mem:///b"), e("mem:///c")],
    /// );
    /// p.set_mark(2, true); // marca previa al gesto
    /// p.begin_sweep();
    /// p.apply_sweep(0, 1);
    /// assert_eq!(p.marks_len(), 3);
    /// p.revert_sweep();
    /// assert_eq!(p.marks_len(), 1, "solo sobrevive la marca previa");
    /// ```
    pub fn revert_sweep(&mut self) {
        let extent = self.sweep_extent.take();
        let mut soltar: Vec<VPath> = Vec::new();
        if let (Some(baseline), Some((lo, hi))) = (self.sweep_baseline.as_ref(), extent) {
            let hi = hi.min(self.entries.len().saturating_sub(1));
            for i in lo..=hi {
                let Some(entry) = self.entries.get(i) else {
                    continue;
                };
                // Solo se suelta lo que puso ESTE barrido: lo anterior al
                // gesto está en la baseline y no se toca.
                if !baseline.contains(&entry.path) {
                    soltar.push(entry.path.clone());
                }
            }
        }
        for path in soltar {
            self.marks.remove(&path);
        }
    }

    /// Ends a pointer sweep, releasing its baseline. Idempotent, and not
    /// required for correctness ([`Self::begin_sweep`] re-arms anyway) —
    /// it only stops a mark-set-sized snapshot from outliving the gesture.
    pub fn end_sweep(&mut self) {
        self.sweep_baseline = None;
        self.sweep_extent = None;
    }

    /// Is this index reachable by a mark right now? The VISIBLE subset
    /// under an active [`Mode::Filter`] quick search, any listed index
    /// otherwise — `markable_indices` without materialising it.
    pub(super) fn is_markable(&self, index: usize) -> bool {
        match self.quick_visible() {
            // `vis` viene en orden ASCENDENTE (`nav::matches_folded`
            // enumera `entries` en orden y filtra), invariante clavada por
            // `quick_visible_viene_en_orden_ascendente`: la búsqueda
            // binaria evita un barrido lineal por cada fila del rango.
            Some(vis) => vis.binary_search(&index).is_ok(),
            None => index < self.entries.len(),
        }
    }

    /// Marks (`marked = true`) or unmarks (`false`) ONE entry by its index
    /// in [`Self::entries`] — the primitive a ctrl+click needs, naming a row
    /// directly instead of the cursor ([`Self::toggle_mark`], which only
    /// ever reaches the selection). No-op if the index is outside the
    /// listing.
    ///
    /// MARKING respects the quick filter and UNMARKING does not, and the
    /// asymmetry is deliberate. An index is resolved from a painted frame
    /// against a listing that is not index-stable — an incremental fill
    /// inserts entries, a `refill` prunes them, a re-sort moves them — so by
    /// the time the index arrives here it can name a different entry than
    /// the one under the pointer, possibly one the filter hides. Marking
    /// the wrong entry WIDENS the next bulk operation onto a file nobody
    /// chose; unmarking the wrong entry only ever shrinks it. Only the
    /// first of those can destroy data, so only the first is refused.
    ///
    /// Unmarking down to an empty set re-arms [`Self::marked_paths`]'s
    /// cursor fallback, the same caveat [`Self::mark_glob`] carries.
    pub fn set_mark(&mut self, index: usize, marked: bool) {
        if marked && !self.is_markable(index) {
            return;
        }
        let Some(path) = self.entries.get(index).map(|e| e.path.clone()) else {
            return;
        };
        if marked {
            self.marks.insert(path);
        } else {
            self.marks.remove(&path);
        }
    }

    /// Flips the mark of every entry of the visible set (see
    /// `markable_indices`). Marks OUTSIDE that set SURVIVE untouched:
    /// invert is "flip what you see", not "replace the selection with its
    /// complement" — under a filter, [`Self::marked_paths`] can therefore
    /// still return entries the user is not looking at.
    pub fn invert_marks(&mut self) {
        for i in self.markable_indices() {
            let Some(path) = self.entries.get(i).map(|e| e.path.clone()) else {
                continue;
            };
            if !self.marks.remove(&path) {
                self.marks.insert(path);
            }
        }
    }

    /// Marks (`mark = true`) or unmarks (`false`) the visible entries whose
    /// name matches `pattern`, a glob. Returns how many marks it ADDED or
    /// REMOVED, never the resulting total — a pattern that only re-marks
    /// what was already marked returns 0 even though the selection is
    /// non-empty; read [`Self::marks_len`] for the total.
    ///
    /// Matching folds BOTH sides through the quick-search pipeline
    /// ([`nav::fold_with`](crate::nav::fold_with): lossy UTF-8 → NFC →
    /// lowercase → NFC, honouring the pane's name reinterpretation) before
    /// compiling the glob, so a pattern matches the FOLDED name, not the
    /// text the pane paints: [`crate::display_name_with`] additionally
    /// MASKS bidi overrides and invisibles to U+FFFD, which the fold does
    /// not — a name typed exactly as painted only matches if it is already
    /// NFC, lowercase, and free of masked characters. Folding the pattern
    /// is what makes NFD and uppercase input match: the fold is the ONE
    /// definition of name equality, shared with the quick search. The glob
    /// deliberately does NOT add `case_insensitive` on top — regex-crate
    /// case folding is wider than the fold (`s` would match `ſ` U+017F)
    /// and would mark files the quick search considers distinct.
    ///
    /// A non-UTF-8 name's invalid bytes fold to U+FFFD and cannot be named
    /// INDIVIDUALLY — but typing U+FFFD in the pattern names ALL of them at
    /// once, matching every hostile name whose lossy form collapses there.
    /// [`Self::toggle_mark`] always reaches an entry by hand regardless, and
    /// [`Self::marked_paths`] returns each mark's original bytes untouched
    /// (hard rule 1).
    ///
    /// `?` and a character class (`[...]`) count CHARACTERS (#110): the
    /// glob's byte-mode regex is recompiled in Unicode mode
    /// (`unicode_glob_regex`), so `a?o` matches `año` even though `ñ` is
    /// two bytes. Unmarking down to an empty set re-arms
    /// [`Self::marked_paths`]'s cursor fallback (it returns the entry under
    /// the cursor when no marks remain) — a caller must read the count this
    /// method returns rather than assume the mark set still reflects what
    /// the user last saw.
    ///
    /// # Errors
    /// [`PatternError::Glob`] if the pattern does not compile. Nothing is
    /// marked in that case.
    pub fn mark_glob(&mut self, pattern: &str, mark: bool) -> Result<usize, PatternError> {
        // El patrón se pliega con el MISMO pipeline que el nombre (#103): el
        // fold es Unicode, `case_insensitive` de globset es solo-ASCII
        // (emite `(?-u)`), así que sin plegar la aguja un patrón NFD o una
        // mayúscula no-ASCII no casarían NADA en silencio.
        let folded = crate::nav::fold(pattern.as_bytes());
        // SIN `case_insensitive`: el fold ya minusculiza AMBOS lados, y el
        // `(?i)` del regex Unicode es case-folding MÁS ANCHO que el fold
        // (`s` casaría `ſ` U+017F, `μ` casaría `µ` U+00B5) — marcaría
        // ficheros que el quick search considera distintos. UNA sola
        // definición de igualdad: la del fold (audit #110).
        let glob = GlobBuilder::new(&folded)
            .backslash_escape(true) // si no, la semántica de `\` depende del SO (globset la
            // hace depender de `is_separator('\\')`, true en unix, false en
            // windows) — `\` es un byte de nombre legal en Linux (corpus
            // `win_backslash`) y el patrón debe casarlo igual en las dos.
            .build()
            .map_err(|e| PatternError::Glob(e.to_string()))?;
        // Modo Unicode (#110): `?`/clases cuentan CARACTERES, no bytes.
        // `size_limit` porque esto es API pública sin tope propio (el modal
        // de la TUI acota a 256 chars, pero nada obliga a otros callers);
        // el motor de `regex` es lineal, así que el guard es de memoria del
        // programa compilado, no de backtracking. `dot_matches_new_line`:
        // globset compila su matcher con ese flag y `*`/`?` traducen a
        // `.`-derivados — sin él, un nombre con `\n` (byte legal en unix,
        // corpus `control_newline`) dejaría de casar `*` EN SILENCIO.
        let matcher = regex::RegexBuilder::new(&unicode_glob_regex(&glob)?)
            .size_limit(1 << 20)
            .dot_matches_new_line(true)
            .build()
            .map_err(|e| PatternError::Glob(e.to_string()))?;
        let enc = self.name_encoding;
        let mut changed = 0usize;
        for i in self.markable_indices() {
            let Some(entry) = self.entries.get(i) else {
                continue;
            };
            let name = entry.path.file_name().map_or(&b""[..], |n| n.as_bytes());
            if !matcher.is_match(crate::nav::fold_with(name, enc).as_str()) {
                continue;
            }
            let path = entry.path.clone();
            let hit = if mark {
                self.marks.insert(path)
            } else {
                self.marks.remove(&path)
            };
            if hit {
                changed += 1;
            }
        }
        Ok(changed)
    }

    /// Total size of every marked entry that is NOT a directory, saturating.
    /// A symlink contributes its own size, never its target's. Directories
    /// contribute 0: nothing here walks a tree, and a status bar that added
    /// a directory's own inode size would be claiming a total it never
    /// computed.
    #[must_use]
    pub fn marked_bytes(&self) -> u64 {
        self.entries
            .iter()
            .filter(|e| e.kind != EntryKind::Dir && self.marks.contains(&e.path))
            .fold(0u64, |acc, e| acc.saturating_add(e.size.unwrap_or(0)))
    }

    /// How many marked entries are directories. [`Self::marked_bytes`]
    /// deliberately excludes directories (nothing here walks a tree), so a
    /// status bar that renders `marked_bytes` alone would understate a
    /// selection that includes one: a marked 10-byte file plus a 40 GiB
    /// directory must not read as "2 marked, 10 B" — that reads like a
    /// transfer size and is not one. Callers name the directory count
    /// separately instead of folding it into a total nobody computed.
    #[must_use]
    pub fn marked_dirs(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.kind == EntryKind::Dir && self.marks.contains(&e.path))
            .count()
    }

    /// Marks dropped by the last [`Self::refill`] because their entry was
    /// gone (#103) — see the `pruned_marks` field. Zero after a `cd`
    /// ([`Self::set_listing`]/[`Self::begin_loading`]) or when nothing was
    /// pruned. A later task surfaces this in the status bar; this accessor
    /// alone adds no UI.
    #[must_use]
    pub fn pruned_marks(&self) -> usize {
        self.pruned_marks
    }

    /// Drops marks whose entry is no longer listed and returns how many were
    /// dropped. A mark is a claim about an entry that EXISTS: a stale path
    /// would silently widen the next bulk operation. Called from
    /// [`Self::refill`], the same-dir refresh: the only path that can drop an
    /// entry without a `cd`. A paginated fill ([`Self::extend`], ADR 0017)
    /// only ADDS entries, so a mark placed mid-fill always points at
    /// something present and needs no pruning there.
    ///
    /// Accepted TOCTOU: identity here is the byte-exact `VPath` alone (hard
    /// rule 1) — the entry's `kind` is not part of it. If an external actor
    /// deletes a marked file and recreates a directory at the same path
    /// between listings, the mark survives the prune and a bulk operation
    /// acts on whatever now lives at that path, file or directory.
    pub(super) fn prune_marks(&mut self) -> usize {
        if self.marks.is_empty() {
            return 0;
        }
        let before = self.marks.len();
        let present: HashSet<&VPath> = self.entries.iter().map(|e| &e.path).collect();
        self.marks.retain(|p| present.contains(p));
        before - self.marks.len()
    }
}
