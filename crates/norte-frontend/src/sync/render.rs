//! El plan, en celdas.
//!
//! Toma un paso o un fallo y devuelve las columnas ya saneadas y acotadas —
//! con sus glifos y su reinterpretación de nombres— para que cada superficie
//! solo tenga que colocarlas. Lo que NO hace es decidir: eso ya venía
//! decidido.

use norte_proto::methods::{DestTrash, SyncReason, SyncStep};
use unicode_normalization::UnicodeNormalization;

use super::{
    RelAnchor, RelDisplay, StepUndo, anchor_for, anchor_of, rel_display, step_glyph, step_undo,
    undo_glyph,
};

/// Las reinterpretaciones de nombres (#57) de los dos lados de una
/// sincronización.
///
/// Una struct con dos campos NOMBRADOS y no una tupla `(Option<_>,
/// Option<_>)`: los dos valores son del mismo tipo, así que trasponerlos
/// compila — y trasponerlos ES el #152, un `dest_rel` decodificado con el
/// codepage del ORIGEN, o sea nombrando otros bytes que el fichero sobre el
/// que cae la escritura. Aquí el compilador no ayuda; el nombre sí.
///
/// El default —ninguna de las dos— es lo correcto para un frontend que no
/// tiene overrides por ubicación, como el CLI: los nombres se leen como
/// vienen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncEncodings {
    /// La del lado ORIGEN.
    pub source: Option<norte_encoding::NameEncoding>,
    /// La del lado DESTINO, que puede ser otra: los dos panes son dos
    /// ubicaciones y pueden llevar overrides distintos.
    pub dest: Option<norte_encoding::NameEncoding>,
}

impl SyncEncodings {
    /// Con cuál de las dos se lee una ruta anclada en `anchor`.
    ///
    /// Es la mitad del #152 que no estaba escrita en ninguna parte: la
    /// ortografía del destino se leía con la del destino (eso ya lo hacía cada
    /// frontend a mano), pero el `rel` de un [`norte_proto::methods::SyncStepKind::DeleteTree`]
    /// —que cuelga del DESTINO, ver [`crate::sync::anchor_of`]— se leía con la del ORIGEN.
    /// Con dos panes con overrides distintos, eso nombra el subárbol que se va
    /// a borrar con el codepage del árbol que NO se toca, en la pantalla donde
    /// se aprueba borrarlo.
    ///
    /// [`RelAnchor::Either`] se lee con la del origen, que es de donde cuelga
    /// «casi siempre» un `rel` (normativo en el wire): no se sabe, y elegir la
    /// otra no sería más cierto — lo que un pane no debe hacer con un `Either`
    /// es afirmar la COLUMNA, y eso lo dice [`StepCells::anchor`].
    ///
    /// Dos cosas hacen ese brazo menos peligroso de lo que parece, y las dos
    /// se pierden si no se escriben:
    ///
    /// * la elección solo CAMBIA algo para bytes que no son UTF-8 válido
    ///   (`display_name_with` no reinterpreta el UTF-8 válido), y en ese caso
    ///   el resultado es SIEMPRE `hostile = true` — o sea que un `Either`
    ///   leído con el codepage del otro lado llega marcado como «este texto no
    ///   son los bytes» a las dos superficies;
    /// * **para un PASO**, el único camino destructivo hasta `Either` es un
    ///   [`norte_proto::methods::SyncStepKind::Unknown`] ([`crate::sync::anchor_of`]), y un solo paso así deja el
    ///   plan en [`crate::sync::PlanIntegrity::Unnameable`], que no se puede aprobar. Lo
    ///   que queda bajo `Either` es un `Skip`, que no escribe nada.
    ///
    /// **Ese segundo punto NO vale para un FALLO del informe**
    /// ([`render_failure`]), y decirlo importa: un `DeleteTree` que falla por
    /// permisos contra un destino de solo lectura es la fila más corriente de
    /// un `Mirror`, su `rel` cuelga del DESTINO, y el informe no trae la clase
    /// que lo diría. Ahí este brazo sí puede nombrar un subárbol del destino
    /// con el codepage del árbol que no se toca. Lo que lo acota es que el
    /// resultado llega `hostile = true` y que el ancla se PINTA.
    ///
    /// ```
    /// use norte_frontend::sync::{RelAnchor, SyncEncodings};
    /// use norte_encoding::NameEncoding;
    /// let enc = SyncEncodings {
    ///     source: Some(NameEncoding::Cp437),
    ///     dest: None,
    /// };
    /// // Un `DeleteTree` habla del DESTINO, aunque se pinte en la primera
    /// // columna.
    /// assert_eq!(enc.for_anchor(RelAnchor::Dest), None);
    /// assert_eq!(enc.for_anchor(RelAnchor::Source), Some(NameEncoding::Cp437));
    /// // Y lo que no consta se lee como el origen, que es de donde cuelga
    /// // «casi siempre» un `rel`.
    /// assert_eq!(enc.for_anchor(RelAnchor::Either), Some(NameEncoding::Cp437));
    /// ```
    #[must_use]
    pub fn for_anchor(self, anchor: RelAnchor) -> Option<norte_encoding::NameEncoding> {
        match anchor {
            RelAnchor::Dest => self.dest,
            RelAnchor::Source | RelAnchor::Either => self.source,
        }
    }
}

/// The three glyphs a step paints: what it does, how sure the comparison was,
/// and whether it comes back.
///
/// Three and not two, and the third is the one that needed a wire field: see
/// the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StepGlyphs {
    /// What the step does ([`step_glyph`]).
    pub kind: char,
    /// What the comparison's verdict is worth
    /// ([`crate::compare::confidence_glyph`] — the same marks as the diff
    /// pane, because it is the same question).
    pub confidence: char,
    /// Whether the undo gives it back ([`undo_glyph`]).
    pub undo: char,
}

/// Everything a painter needs for one step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepCells {
    /// The step's id, for a cursor to anchor to.
    pub id: u64,
    /// The three marks.
    pub glyphs: StepGlyphs,
    /// Which root [`StepCells::rel`] hangs from.
    pub anchor: RelAnchor,
    /// The path, masked.
    pub rel: RelDisplay,
    /// The DESTINATION's own spelling, when its bytes differ from `rel`'s
    /// (#152). `Some` means the two sides spell one entry two ways and the
    /// pane must show both: the write lands on THIS one.
    pub dest_rel: Option<RelDisplay>,
    /// `dest_rel` is `Some` AND its NFC form matches `rel`'s NFC form, even
    /// though the bytes AND the `String`s differ — an NFC/NFD pair
    /// (precomposed `café.txt` vs `café.txt` spelled with a combining
    /// acute) is the canonical case: not `String`-equal (`'é'` is one
    /// `char`, `'e' + '\u{301}'` is two), valid UTF-8 on both sides so
    /// neither half is `hostile`, and rendered to the SAME glyph by any font
    /// that composes combining marks. Nothing else says the pane is not just
    /// repeating itself (#192). See [`crate::sync::dest_twin_label`].
    pub dest_rel_twin: bool,
    /// Bytes the step moves, when the provider said.
    pub size: Option<u64>,
    /// What the undo would do with it.
    pub undo: StepUndo,
    /// Why it is a `Skip` or why it cannot be undone.
    pub reason: Option<SyncReason>,
}

/// One step, ready to paint.
///
/// `dest_trash` is not optional and not defaulted: without it the undo column
/// cannot be computed, and a renderer that reads [`SyncStep::reversal`] on its
/// own is exactly the bug this module exists to prevent.
///
/// Each path is masked with the reinterpretation of the side it hangs from
/// ([`SyncEncodings::for_anchor`]), which is a decision no caller has to make
/// again: `dest_rel` is ALWAYS the destination's spelling (#152), and a
/// `DeleteTree`'s `rel` is a destination path too even though it sits in the
/// first column.
///
/// ```
/// use norte_frontend::sync::{StepUndo, SyncEncodings, render_step};
/// use norte_proto::methods::{DestTrash, SyncStepKind};
/// # use norte_proto::methods::{CompareConfidence, CompareCriterion, RelPath, StepReversal,
/// #     SyncStep};
/// let step = SyncStep {
///     id: 3,
///     kind: SyncStepKind::Copy,
///     rel: RelPath::parse_wire("a.txt").expect("rel"),
///     dest_rel: None,
///     size: Some(10),
///     criterion: CompareCriterion::Presence,
///     confidence: CompareConfidence::Certain,
///     reversal: Some(StepReversal::Delete),
///     reason: None,
/// };
/// let cells = render_step(&step, DestTrash::Absent, SyncEncodings::default());
/// assert_eq!(cells.undo, StepUndo::LeftBehind);
/// ```
#[must_use]
pub fn render_step(step: &SyncStep, dest_trash: DestTrash, enc: SyncEncodings) -> StepCells {
    let undo = step_undo(step, dest_trash);
    let anchor = anchor_of(step);
    let rel = rel_display(&step.rel, enc.for_anchor(anchor));
    // Siempre con la del DESTINO, sea cual sea el ancla: `dest_rel` existe
    // precisamente para enseñar la ortografía de allí, que es sobre la que
    // cae la escritura.
    //
    // Y se pliega AQUÍ cuando los BYTES coinciden, no en cada pintor y no
    // por el texto pintado: `RelDisplay::text` es lossy, así que
    // `caf\xe9.txt` y `caf\x82.txt` —dos ficheros distintos— son el mismo
    // `caf\u{FFFD}.txt`, y un pintor que compare textos esconde justo el
    // campo que existe para decir sobre qué nombre cae la escritura
    // (#152). El wire ya compara por bytes
    // (`SyncStep::shape_is_consistent`); esto es la misma regla, una sola
    // vez, para los tres frontends.
    let dest_rel = step
        .dest_rel
        .as_ref()
        .filter(|d| **d != step.rel)
        .map(|r| rel_display(r, enc.dest));
    // #192: los BYTES ya distinguen las dos rutas (si no, `dest_rel` sería
    // `None`), pero pueden RENDERIZAR igual de todas formas — un par NFC/NFD
    // es UTF-8 válido en las dos mitades, así que ninguna llega `hostile`, y
    // ni siquiera `text == text` lo detecta: "é" precompuesta y "e" + acento
    // combinante son Strings DISTINTOS que una fuente compone al mismo
    // glifo. Por NFC y no por bytes NI por igualdad de String a secas —la
    // comparación es la única parte de esto que normaliza; `RelDisplay::text`
    // en sí sigue siendo el enmascarado byte-exacto de siempre.
    let dest_rel_twin = dest_rel
        .as_ref()
        .is_some_and(|d| d.text.nfc().eq(rel.text.nfc()));
    StepCells {
        id: step.id,
        glyphs: StepGlyphs {
            kind: step_glyph(step.kind),
            confidence: crate::compare::confidence_glyph(step.confidence),
            undo: undo_glyph(undo),
        },
        anchor,
        rel,
        dest_rel,
        dest_rel_twin,
        size: step.size,
        undo,
        reason: step.reason,
    }
}

/// Una fila del informe (`sync.report`), ya resuelta: las dos rutas leídas con
/// la reinterpretación que le toca a cada una, y la del destino PLEGADA cuando
/// los bytes coinciden.
///
/// Gemela de [`StepCells`], y separada de ella porque un
/// [`norte_proto::methods::SyncFailure`] no es un paso: no lleva clase, así que
/// no hay glifos que pintar ni undo que juzgar. Lo que sí comparte es lo que se
/// puede equivocar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureCells {
    /// La ruta del fallo, enmascarada y badgeada.
    pub rel: RelDisplay,
    /// La ortografía del DESTINO, si el informe la manda y DIFIERE en bytes.
    pub dest_rel: Option<RelDisplay>,
    /// Gemelo de [`StepCells::dest_rel_twin`], y por el mismo motivo (#192):
    /// `dest_rel` es `Some` pero pinta IGUAL que `rel` — un par NFC/NFD, por
    /// ejemplo, es UTF-8 válido en las dos mitades y ninguna llega `hostile`.
    pub dest_rel_twin: bool,
    /// De qué raíz cuelga [`FailureCells::rel`]: [`RelAnchor::Source`] cuando
    /// el informe manda `dest_rel`, y [`RelAnchor::Either`] cuando no — ver
    /// [`render_failure`]. **Un pintor tiene que pintarlo**: en un panel donde
    /// una ruta sin calificar significa «del origen», callar un `Either` es
    /// afirmar el origen.
    pub anchor: RelAnchor,
}

/// Resuelve UNA fila del informe, con las mismas dos reglas que
/// [`render_step`] y por los mismos dos motivos.
///
/// * **El plegado es por BYTES**, no por el texto pintado: `RelDisplay::text`
///   es lossy, así que dos ficheros distintos con un byte inválido cada uno se
///   pintan igual — y comparando textos, el campo que dice sobre qué nombre
///   cayó la escritura desaparece justo cuando los nombres son adversarios.
/// * **La ortografía del destino se lee con la del destino**, sea cual sea el
///   ancla (#152): existe precisamente para nombrar el fichero de allí.
///
/// # El ancla de un fallo casi nunca consta, y entonces es `Either`
/// [`SyncStep::rel`] es «casi siempre» del origen y [`crate::sync::anchor_of`] usa la CLASE
/// del paso para saber cuándo no lo es —un `DeleteTree` habla del destino—.
/// Un [`norte_proto::methods::SyncFailure`] no lleva clase: el informe se lee
/// sin el plan delante. Queda UNA prueba, y es la misma que usa
/// [`crate::sync::anchor_of`]: si el informe manda `dest_rel`, entonces `rel` es la mitad
/// del ORIGEN de la pareja (misma regla y mismo campo, ver
/// [`norte_proto::methods::SyncFailure::dest_rel`]). Sin `dest_rel` no se
/// sabe, y decir «origen» sería justo lo que [`RelAnchor::Either`] existe
/// para no hacer — **un `DeleteTree` que falla por permisos es la fila hostil
/// MÁS común de un `Mirror`**, y su `rel` cuelga del destino.
///
/// Lo que un pintor NO puede hacer con un `Either` es callarse: en un panel
/// donde una ruta sin calificar significa «del origen» (así lo escribe
/// [`StepCells::anchor`]), el silencio es la afirmación. El ancla viaja en
/// [`FailureCells::anchor`] para que se pinte, y la auditoría de encoding de
/// esta fase (MAJOR-2) es exactamente eso.
///
/// La DECODIFICACIÓN de un `Either` sigue siendo la del origen
/// ([`SyncEncodings::for_anchor`]) porque no hay nada mejor que elegir; con
/// dos overrides #57 distintos eso puede nombrar un subárbol del destino con
/// el codepage del árbol que no se tocó, y llega marcado como hostil pero no
/// como «del otro lado».
///
/// **Desde 0.42.0 hay con qué cerrarlo, y esta función todavía no lo usa**
/// (#195 lo puso en el wire, #208 lo consume):
/// [`norte_proto::methods::SyncFailure::kind`] lleva la clase que el core tenía
/// en la mano y tiraba, así que un `DeleteTree` que falló ya se puede anclar en
/// el DESTINO con la misma regla que [`crate::sync::anchor_of`] aplica a un paso, en vez de
/// caer en `Either`. Cambiar lo que este módulo devuelve cambia lo que dos
/// frontends pintan, así que no viaja en el bump del wire.
///
/// ```
/// use norte_frontend::sync::{RelAnchor, SyncEncodings, render_failure};
/// use norte_proto::methods::{RelPath, SyncFailure, SyncFailureCause, SyncStepKind};
/// let f = SyncFailure {
///     rel: RelPath::parse_wire("sub/a.txt").expect("rel"),
///     dest_rel: Some(RelPath::parse_wire("sub/a.txt").expect("rel")),
///     cause: SyncFailureCause::Denied,
///     kind: SyncStepKind::Copy,
/// };
/// let cells = render_failure(&f, SyncEncodings::default());
/// assert_eq!(cells.rel.text, "sub/a.txt");
/// assert!(cells.dest_rel.is_none(), "la misma ortografía no se repite");
/// // Con `dest_rel` en el wire, `rel` es la mitad del ORIGEN de la pareja —
/// // aunque las dos ortografías coincidan y no haya nada que pintar aparte.
/// assert_eq!(cells.anchor, RelAnchor::Source);
///
/// // Sin `dest_rel` no hay prueba, y eso NO es «del origen»: el `rel` de un
/// // `DeleteTree` que falló cuelga del destino.
/// let solo = SyncFailure {
///     rel: RelPath::parse_wire("viejo").expect("rel"),
///     dest_rel: None,
///     cause: SyncFailureCause::Io,
///     kind: SyncStepKind::DeleteTree,
/// };
/// // …y desde 0.42.0 el wire lo dice (`kind`), así que esta función lo LEE
/// // (#208): un `DeleteTree` habla del destino, con `dest_rel` o sin él.
/// assert_eq!(
///     render_failure(&solo, SyncEncodings::default()).anchor,
///     RelAnchor::Dest
/// );
/// ```
#[must_use]
pub fn render_failure(
    failure: &norte_proto::methods::SyncFailure,
    enc: SyncEncodings,
) -> FailureCells {
    // #208: la MISMA regla que un paso (`anchor_of`), ahora que 0.42.0 pone la
    // clase en el wire. La fila hostil más común de un espejo —un borrado
    // rechazado por permisos, sin `dest_rel`, con `rel` medido contra el
    // destino— deja de ser `Either`, que es lo que hacía que
    // `SyncEncodings::for_anchor` la decodificara con la reinterpretación del
    // ÁRBOL QUE NO SE TOCÓ (hallazgo del encoding-auditor).
    let anchor = anchor_for(failure.kind, failure.dest_rel.is_some(), None);
    let rel = rel_display(&failure.rel, enc.for_anchor(anchor));
    let dest_rel = failure
        .dest_rel
        .as_ref()
        .filter(|d| **d != failure.rel)
        .map(|r| rel_display(r, enc.dest));
    // #192, la misma regla que `render_step`: por NFC, no por igualdad de
    // `String` a secas — "é" precompuesta y "e" + acento combinante son
    // Strings distintos que rinden al mismo glifo.
    let dest_rel_twin = dest_rel
        .as_ref()
        .is_some_and(|d| d.text.nfc().eq(rel.text.nfc()));
    FailureCells {
        rel,
        dest_rel,
        dest_rel_twin,
        anchor,
    }
}
