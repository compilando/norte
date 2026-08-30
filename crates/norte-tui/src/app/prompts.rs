//! Los prompts de una línea que `App` abre sobre el listado: marcar por
//! patrón, renombrar, empaquetar, partir, crear directorio, destino de una
//! transferencia, línea de comandos, renombrado con IA y búsqueda semántica.
//! Todos siguen la misma forma: `open_*`, `*_push`, `*_pop`, `cancel_*`,
//! `*_confirm`, `*_submitted` y `*_set_error`.

use super::modal::{Modal, PromptKind, TextPrompt, TransferKind};
use super::{AI_RENAME_PAIR_LIMIT, App, SEMANTIC_HIT_LIMIT, format_by_name, parse_size};
use norte_i18n::{t, ta};
use norte_proto::{EntryKind, VPath};

impl App {
    /// Teclea en el prompt `kind`, si es el que está abierto. No-op si no lo
    /// es: cada tabla de despacho llama a la suya y una tecla no debe
    /// escribir en el campo de otro modal.
    pub(crate) fn prompt_push(&mut self, kind: PromptKind, c: char) {
        if let Some(prompt) = self.open_prompt(kind) {
            prompt.push(c);
        }
    }

    /// Borra hacia atrás en el prompt `kind`. No-op si no es el abierto.
    pub(crate) fn prompt_pop(&mut self, kind: PromptKind) {
        if let Some(prompt) = self.open_prompt(kind) {
            prompt.pop();
        }
    }

    /// Deja el diagnóstico de un submit fallido y CONSERVA lo tecleado: el
    /// usuario corrige y reintenta.
    fn prompt_set_error(&mut self, kind: PromptKind, msg: String) {
        if let Some(prompt) = self.open_prompt(kind) {
            prompt.set_error(msg);
        }
    }

    /// Cierra el prompt `kind` tras ENCOLAR lo que pedía, y abre el pendiente
    /// siguiente. La disciplina es la misma en los diez: `*_confirm` valida
    /// y NO cierra; cierra esto, y solo cuando el submit salió.
    fn prompt_submitted(&mut self, kind: PromptKind) {
        if self.modal.as_ref().and_then(Modal::prompt_kind) == Some(kind) {
            self.modal = None;
            self.open_next_pending();
        }
    }

    /// Esc sobre un prompt de texto: cierra sin escribir nada y abre el
    /// pendiente siguiente.
    ///
    /// Solo los modales de TEXTO LIBRE se cierran por aquí. Uno de DECISIÓN
    /// —colisión, aprobación, TOFU— tiene que denegar por `on_dialog_key`, y
    /// llegar aquí con uno abierto es un bug de enrutado: se asegura en debug
    /// y se ignora en release, jamás se cierra a ciegas la decisión de otro.
    pub(crate) fn cancel_prompt(&mut self, kind: PromptKind) {
        if self.modal.as_ref().and_then(Modal::prompt_kind) != Some(kind) {
            debug_assert!(
                false,
                "solo los modales de texto libre se cierran sin decisión; \
                 un modal de DECISIÓN debe denegar por on_dialog_key"
            );
            return;
        }
        self.modal = None;
        self.open_next_pending();
    }

    /// El campo de texto del prompt `kind`, si es justo el que está abierto.
    fn open_prompt(&mut self, kind: PromptKind) -> Option<TextPrompt<'_>> {
        let modal = self.modal.as_mut()?;
        (modal.prompt_kind() == Some(kind)).then(|| modal.text_prompt())?
    }
}

impl App {
    /// Abre el modal de marcado por patrón (#103).
    pub fn open_mark_pattern(&mut self, mark: bool) {
        self.modal = Some(Modal::MarkPattern {
            mark,
            pattern: String::new(),
            error: None,
        });
    }

    /// Añade un carácter al patrón en curso. No-op sin modal de patrón.
    pub fn mark_pattern_push(&mut self, c: char) {
        self.prompt_push(PromptKind::MarkPattern, c);
    }

    /// Borra el último carácter del patrón. No-op sin modal de patrón.
    pub fn mark_pattern_pop(&mut self) {
        self.prompt_pop(PromptKind::MarkPattern);
    }

    /// Aplica el patrón: cierra el modal y devuelve cuántas marcas cambió.
    /// Un patrón inválido DEJA el modal abierto con el diagnóstico — el
    /// usuario conserva lo tecleado para corregirlo.
    ///
    /// # Errors
    /// Si el glob no compila.
    pub fn mark_pattern_confirm(&mut self) -> Result<usize, norte_frontend::PatternError> {
        let Some(Modal::MarkPattern { mark, pattern, .. }) = &self.modal else {
            return Ok(0);
        };
        let (mark, pattern) = (*mark, pattern.clone());
        match self.focused_mut().mark_glob(&pattern, mark) {
            Ok(changed) => {
                self.modal = None;
                // Misma disciplina que CUALQUIER otro cierre de modal
                // (`on_dialog_key`, `cancel_mark_pattern`): jamás dejar una
                // aprobación/colisión encolada esperando a la próxima tecla.
                self.open_next_pending();
                Ok(changed)
            }
            Err(e) => {
                let msg = e.to_string();
                if let Some(Modal::MarkPattern { error, .. }) = &mut self.modal {
                    *error = Some(msg);
                }
                Err(e)
            }
        }
    }

    /// Abre el rename in situ (shift+F6, #105): Move con destino en el
    /// PADRE del propio `from` — no el dir del pane, que en el pane VIRTUAL
    /// de búsqueda es la raíz del walk y renombraría moviendo el hit de
    /// sitio. Siempre sobre el cursor (las marcas no renombran en bloque —
    /// eso sería un batch-rename, otra feature). No-op sobre una raíz.
    pub fn open_rename(&mut self) {
        let Some(from) = self.focused().selected().map(|e| e.path.clone()) else {
            return;
        };
        let Some(to_dir) = from.parent() else {
            return;
        };
        self.open_transfer_name_with(TransferKind::Move, self.focus, from, to_dir, false);
    }

    /// El modal de nombre editable. `from_pane` es el pane de ORIGEN y no se
    /// da por hecho que sea el que tiene el foco: un drop nace en el pane
    /// donde bajó el botón, y de ahí sale la reinterpretación de nombres
    /// (#57) con la que se siembra el campo.
    pub(super) fn open_transfer_name_with(
        &mut self,
        kind: TransferKind,
        from_pane: usize,
        from: VPath,
        to_dir: VPath,
        from_marks: bool,
    ) {
        let original = from
            .file_name()
            .map_or(Vec::new(), |n| n.as_bytes().to_vec());
        let enc = self.panes[from_pane].name_encoding();
        // Prefill = lo que el pane PINTA (#98/M1): bajo reinterpretación,
        // un nombre no-UTF8 se decodifica (#57) en vez de pasar por lossy
        // — editar produce el texto que se VE; sin tocar siguen mandando
        // los bytes originales.
        let name = match (enc, std::str::from_utf8(&original)) {
            (_, Ok(s)) => s.to_owned(),
            (Some(e), Err(_)) => norte_encoding::decode_name(&original, e),
            (None, Err(_)) => String::from_utf8_lossy(&original).into_owned(),
        };
        self.modal = Some(Modal::TransferName {
            kind,
            from,
            to_dir,
            name,
            original,
            touched: false,
            from_marks,
            enc,
            error: None,
        });
    }

    /// Añade un carácter al nombre en curso (#105). Marca `touched`: desde
    /// el primer edit, el nombre es el TEXTO. No-op sin el modal.
    pub fn transfer_name_push(&mut self, c: char) {
        self.prompt_push(PromptKind::TransferName, c);
    }

    /// Borra el último carácter (#105). Marca `touched` SOLO si borró algo
    /// (review MINOR-5: un pop vacío no debe estrechar la vía de bytes
    /// originales).
    pub fn transfer_name_pop(&mut self) {
        self.prompt_pop(PromptKind::TransferName);
    }

    /// Cancela sin transferir — mismo contrato guarded que
    /// [`Self::cancel_mkdir`].
    pub fn cancel_transfer_name(&mut self) {
        self.cancel_prompt(PromptKind::TransferName);
    }

    /// Valida y devuelve `(kind, from, dest)` SIN cerrar el modal (misma
    /// disciplina que [`Self::mkdir_confirm`]: cierra el submit que encoló,
    /// vía [`Self::transfer_name_submitted`]). Reglas: sin tocar → los
    /// BYTES originales (regla 1); tocado → los bytes del texto, y un texto
    /// que aún contiene U+FFFD (residuo del prefill lossy de un nombre
    /// hostil) se RECHAZA — confirmarlo escribiría mojibake real en disco.
    /// El guard no distingue residuo de intención: también un U+FFFD
    /// TECLEADO a propósito se rechaza (asimetría deliberada con el mkdir,
    /// que no tiene prefill lossy del que heredar residuos). Bajo
    /// reinterpretación (#57), un nombre TOCADO escribe los bytes UTF-8 del
    /// texto decodificado — transcodifica a propósito: «ver el nombre bien
    /// y arreglarlo» es el caso de uso, y el intocado sigue byte-exacto.
    /// `dest == from` también se rechaza (no-op; en rename, «mismo
    /// nombre»). El nombre pasa por [`norte_proto::Segment`] (ni vacío, ni
    /// `/`, ni NUL, ni `.`/`..`).
    pub fn transfer_name_confirm(&mut self) -> Option<(TransferKind, VPath, VPath)> {
        let Some(Modal::TransferName {
            kind,
            from,
            to_dir,
            name,
            original,
            touched,
            ..
        }) = &self.modal
        else {
            return None;
        };
        let bytes = if *touched {
            if name.contains('\u{FFFD}') {
                let msg = norte_i18n::t("msg-transfer-name-fffd");
                self.transfer_name_set_error(msg);
                return None;
            }
            name.as_bytes().to_vec()
        } else {
            original.clone()
        };
        let (kind, from, to_dir) = (*kind, from.clone(), to_dir.clone());
        match norte_proto::Segment::new(bytes) {
            Ok(seg) => {
                let dest = to_dir.join(seg);
                if dest == from {
                    self.transfer_name_set_error(norte_i18n::t("msg-transfer-name-same"));
                    return None;
                }
                Some((kind, from, dest))
            }
            Err(e) => {
                self.transfer_name_set_error(e.to_string());
                None
            }
        }
    }

    /// Cierra el modal tras un submit que SÍ encoló (#105) y, si el origen
    /// era la MARCA, la CONSUME (review MAJOR-1 — misma doctrina que el
    /// lote: la selección se consume al ENVIAR). Esc y los fallos jamás
    /// consumen.
    pub fn transfer_name_submitted(&mut self) {
        if let Some(Modal::TransferName { from_marks, .. }) = &self.modal {
            // Lo que este submit tiene de propio, y por lo que no es el
            // cierre genérico a secas: el lote se consume al ENVIAR.
            if *from_marks {
                self.focused_mut().clear_marks();
            }
        }
        self.prompt_submitted(PromptKind::TransferName);
    }

    /// Deja el diagnóstico de un intento fallido (#105): el texto tecleado
    /// sobrevive para corregir.
    pub fn transfer_name_set_error(&mut self, msg: String) {
        self.prompt_set_error(PromptKind::TransferName, msg);
    }

    /// Abre el diálogo de empaquetar (#132), o deja el motivo si no se puede.
    ///
    /// El nombre por defecto sale de lo que se va a empaquetar: con una marca
    /// sola o el cursor encima, el de esa entrada; con varias, el del
    /// directorio. Es lo que hacen los gestores de los que vienen estas
    /// teclas, y ahorra teclear el caso normal.
    ///
    /// Sobre un panel de solo lectura no se abre: el archivo se escribe AHÍ, y
    /// preguntar el nombre para fallar después es hacer teclear para nada.
    pub fn open_pack(&mut self) {
        if self.pane_read_only(self.focus()) {
            self.message = Some(t("msg-pack-read-only"));
            return;
        }
        let marked = self.focused().marked_paths();
        if marked.is_empty() {
            self.message = Some(t("msg-pack-nothing"));
            return;
        }
        let base = if marked.len() == 1 {
            marked[0].file_name().map(|s| s.as_bytes().to_vec())
        } else {
            self.focused()
                .dir()
                .file_name()
                .map(|s| s.as_bytes().to_vec())
        };
        let base = base.unwrap_or_else(|| b"archivo".to_vec());
        // La sugerencia sale de los bytes del origen, y con la
        // REINTERPRETACIÓN activa si la hay (#57): con «ver nombres como
        // cp866» puesto, el pane pinta `Папка` y el diálogo sugería
        // `?????.zip` — el diálogo contradiciendo al panel desde el que se
        // abrió. Lo que no se puede leer se queda en `U+FFFD` y
        // [`Self::pack_confirm`] REHÚSA confirmarlo, igual que el prompt de
        // renombrar: un nombre con el carácter de reemplazo dentro no es el
        // nombre de nadie.
        let suggested = match self.focused().name_encoding() {
            Some(enc) => format!("{}.zip", norte_encoding::decode_name(&base, enc)),
            None => format!("{}.zip", String::from_utf8_lossy(&base)),
        };
        self.modal = Some(Modal::Pack {
            name: suggested,
            error: None,
        });
    }

    /// Añade un carácter al nombre del archivo. No-op sin su modal.
    pub fn pack_push(&mut self, c: char) {
        self.prompt_push(PromptKind::Pack, c);
    }

    /// Borra el último carácter. No-op sin su modal.
    pub fn pack_pop(&mut self) {
        self.prompt_pop(PromptKind::Pack);
    }

    /// Cancela el diálogo de empaquetar sin escribir nada.
    pub fn cancel_pack(&mut self) {
        self.cancel_prompt(PromptKind::Pack);
    }

    /// Los params de `archive.pack` que el diálogo describe, o `None` si el
    /// nombre no vale.
    ///
    /// El FORMATO sale del nombre tecleado y viaja explícito; un nombre sin
    /// extensión conocida se rehúsa aquí en vez de empaquetar en un formato
    /// que el usuario no pidió.
    #[must_use]
    pub fn pack_confirm(&mut self) -> Option<norte_proto::methods::ArchivePackParams> {
        let Some(Modal::Pack { name, .. }) = &self.modal else {
            return None;
        };
        let name = name.clone();
        // El carácter de reemplazo no puede llegar a un nombre de fichero: es
        // lo que queda de unos bytes que no se pudieron leer, y dos nombres
        // distintos producen el MISMO `U+FFFD` — el segundo empaquetado
        // chocaría contra el archivo del primero. Mismo criterio, y misma
        // clave, que el prompt de renombrar.
        if name.contains('\u{FFFD}') {
            self.pack_set_error(t("msg-transfer-name-fffd"));
            return None;
        }
        let Some(format) = format_by_name(name.as_bytes()) else {
            self.pack_set_error(t("msg-pack-unknown-format"));
            return None;
        };
        let Ok(seg) = norte_proto::Segment::new(name.into_bytes()) else {
            self.pack_set_error(t("msg-pack-bad-name"));
            return None;
        };
        let dir = self.focused().dir().clone();
        let dest = dir.join(seg);
        Some(norte_proto::methods::ArchivePackParams {
            sources: self.focused().marked_paths(),
            dest,
            format,
            level: None,
            // La base es el directorio del panel: los nombres guardados son
            // los que se ven en pantalla, que es lo que espera quien luego
            // desempaqueta.
            base: dir,
        })
    }

    /// El diálogo se cerró porque la task encoló.
    pub fn pack_submitted(&mut self) {
        self.prompt_submitted(PromptKind::Pack);
    }

    /// Deja el diagnóstico y conserva lo tecleado.
    pub fn pack_set_error(&mut self, msg: String) {
        self.prompt_set_error(PromptKind::Pack, msg);
    }

    /// El panel al que van los trozos de un split: el siguiente VISIBLE, o el
    /// mismo si no hay otro (#132).
    ///
    /// Por posición visible y no por id de hueco: `slot_ids()` incluye las
    /// pestañas que no están en pantalla, así que los trozos podían aterrizar
    /// en el directorio de una pestaña de fondo — cuatro gigas en un sitio que
    /// el lector no está mirando y que el diálogo no nombra.
    #[must_use]
    pub fn split_dest_pane(&self) -> usize {
        // Por POSICIÓN visible, que es la misma noción de «pane» que usan el
        // foco, `pane_read_only` y el resto de la TUI. Con un solo panel el
        // destino es él mismo — que es lo que hace F5 cuando no hay otro sitio
        // al que apuntar—, no `None`.
        let n = self.panes.len();
        if n <= 1 {
            return self.focus();
        }
        (self.focus() + 1) % n
    }

    /// Abre el diálogo de partir un fichero (#132).
    pub fn open_split(&mut self) {
        // El de solo lectura es el DESTINO, no el de origen: partir lee el
        // panel con foco y escribe en el otro. Con el gate al revés se
        // rehusaba partir un fichero que estuviera en un sitio de solo lectura
        // —dentro de un archivo, en un export SFTP— y se aceptaba partir HACIA
        // uno, que fallaba después con un error crudo.
        let dest = self.split_dest_pane();
        if self.pane_read_only(dest) {
            self.message = Some(t("msg-pack-read-only"));
            return;
        }
        if self
            .focused()
            .selected()
            .is_none_or(|e| e.kind != EntryKind::File)
        {
            self.message = Some(t("msg-split-needs-file"));
            return;
        }
        self.modal = Some(Modal::Split {
            size: "10M".to_owned(),
            error: None,
        });
    }

    /// Añade un carácter al tamaño. No-op sin su modal.
    pub fn split_push(&mut self, c: char) {
        self.prompt_push(PromptKind::Split, c);
    }

    /// Borra el último carácter. No-op sin su modal.
    pub fn split_pop(&mut self) {
        self.prompt_pop(PromptKind::Split);
    }

    /// Cancela el diálogo de partir.
    pub fn cancel_split(&mut self) {
        self.cancel_prompt(PromptKind::Split);
    }

    /// Los params de `file.split` que el diálogo describe, o `None` si el
    /// tamaño no vale.
    #[must_use]
    pub fn split_confirm(&mut self) -> Option<norte_proto::methods::FileSplitParams> {
        let Some(Modal::Split { size, .. }) = &self.modal else {
            return None;
        };
        let Some(bytes) = parse_size(size) else {
            self.split_set_error(t("msg-split-bad-size"));
            return None;
        };
        let path = self.focused().selected().map(|e| e.path.clone())?;
        // Los trozos van al OTRO panel visible si lo hay, y si no al mismo: es
        // lo que hace la copia, y por lo mismo — partir un fichero de un giga
        // en el sitio donde ya está suele no caber. **Sin `?` sobre la
        // búsqueda**: con un solo panel no había «otro», la función entera
        // devolvía `None`, y Enter no hacía absolutamente nada — ni task, ni
        // error, ni cerrar el diálogo.
        let dest_dir = self.panes[self.split_dest_pane()].dir().clone();
        Some(norte_proto::methods::FileSplitParams {
            path,
            part_bytes: bytes,
            dest_dir,
        })
    }

    /// Abre el diálogo de PERMISOS (#314) sobre `targets`, con el campo
    /// prellenado con `mode` si se pudo leer el de la entrada bajo el cursor.
    ///
    /// Prellenar no es un adorno: teclear `755` sobre un campo vacío es fácil,
    /// y quitarle el bit de ejecución a un fichero que ya lo tenía —porque no
    /// se veía cuál era— es la clase de error que este diálogo tiene que hacer
    /// difícil.
    pub fn open_chmod(&mut self, targets: Vec<VPath>, mode: Option<u32>) {
        if targets.is_empty() {
            return;
        }
        self.modal = Some(Modal::Chmod {
            mode: mode
                .map(norte_frontend::chmod::format_mode)
                .unwrap_or_default(),
            targets,
            error: None,
        });
    }

    /// Lo que hay que mandar al confirmar el diálogo de permisos: las rutas y
    /// el modo ya leído. `None` si lo tecleado no es un modo — el diálogo se
    /// queda abierto con su diagnóstico.
    pub fn chmod_confirm(&mut self) -> Option<norte_proto::methods::FsSetModeParams> {
        let Some(Modal::Chmod { mode, targets, .. }) = &self.modal else {
            return None;
        };
        let (texto, paths) = (mode.clone(), targets.clone());
        match norte_frontend::chmod::parse_mode(&texto) {
            Ok(mode) => Some(norte_proto::methods::FsSetModeParams { paths, mode }),
            Err(e) => {
                self.chmod_set_error(norte_i18n::t(e.message_key()));
                None
            }
        }
    }

    /// El diálogo se cerró porque la task encoló.
    pub fn chmod_submitted(&mut self) {
        self.prompt_submitted(PromptKind::Chmod);
    }

    /// Deja el diagnóstico y conserva lo tecleado.
    pub fn chmod_set_error(&mut self, msg: String) {
        self.prompt_set_error(PromptKind::Chmod, msg);
    }

    /// El diálogo se cerró porque la task encoló.
    pub fn split_submitted(&mut self) {
        self.prompt_submitted(PromptKind::Split);
    }

    /// Deja el diagnóstico y conserva lo tecleado.
    pub fn split_set_error(&mut self, msg: String) {
        self.prompt_set_error(PromptKind::Split, msg);
    }

    /// Abre el modal de crear directorio (F7, #104).
    pub fn open_mkdir(&mut self) {
        self.modal = Some(Modal::Mkdir {
            name: String::new(),
            error: None,
        });
    }

    /// Añade un carácter al nombre en curso. No-op sin modal de mkdir.
    /// Tope en `chars` como el patrón (#103): un paste accidental no
    /// desborda el modal; el límite REAL del nombre lo pone el provider.
    pub fn mkdir_push(&mut self, c: char) {
        self.prompt_push(PromptKind::Mkdir, c);
    }

    /// Borra el último carácter del nombre. No-op sin modal de mkdir.
    pub fn mkdir_pop(&mut self) {
        self.prompt_pop(PromptKind::Mkdir);
    }

    /// Cancela `Modal::Mkdir` sin crear nada — el Esc de ESTE modal de
    /// texto libre (mismo contrato y guard que [`Self::cancel_mark_pattern`]:
    /// un modal de DECISIÓN jamás se cierra por aquí).
    pub fn cancel_mkdir(&mut self) {
        self.cancel_prompt(PromptKind::Mkdir);
    }

    /// Valida el nombre y devuelve el DESTINO completo (dir del pane con
    /// foco + nombre como [`norte_proto::Segment`] — la validación es la
    /// del `VPath`: ni vacío, ni `/`, ni NUL, ni `.`/`..`). NO cierra el
    /// modal (#104 review MINOR-1): el caller lo cierra con
    /// [`Self::mkdir_submitted`] SOLO tras encolar la task — un submit que
    /// falla (policy, conexión) deja el diagnóstico con
    /// [`Self::mkdir_set_error`] y el usuario CONSERVA lo tecleado. Un
    /// nombre inválido deja su diagnóstico aquí mismo y devuelve `None`.
    pub fn mkdir_confirm(&mut self) -> Option<VPath> {
        let Some(Modal::Mkdir { name, .. }) = &self.modal else {
            return None;
        };
        match norte_proto::Segment::new(name.as_bytes().to_vec()) {
            Ok(seg) => Some(self.focused().dir().join(seg)),
            Err(e) => {
                let msg = e.to_string();
                self.mkdir_set_error(msg);
                None
            }
        }
    }

    /// Cierra el modal tras un submit que SÍ encoló (#104): misma
    /// disciplina de cierre que el resto (jamás dejar una pendiente
    /// esperando).
    pub fn mkdir_submitted(&mut self) {
        self.prompt_submitted(PromptKind::Mkdir);
    }

    /// Deja el diagnóstico de un submit fallido en el modal (#104): el
    /// nombre tecleado sobrevive para corregir y reintentar.
    pub fn mkdir_set_error(&mut self, msg: String) {
        self.prompt_set_error(PromptKind::Mkdir, msg);
    }

    /// El programa que el bucle debe lanzar en esta vuelta, si alguno.
    ///
    /// **Devuelve `None` cuando el usuario ya pidió salir**, y la intención se
    /// tira: `on_tick` puede ARMAR un `PendingShell` (el editor de
    /// `pane.edit-new`, cuando la creación termina) y a continuación, dentro
    /// del mismo tick, tragarse el `Ctrl+C` que el `refresh_panes` de después
    /// poléa. El bucle drena lo pendiente ANTES de mirar `quit`, así que sin
    /// esta guarda un `Ctrl+C` durante la creación no salía de norte: abría el
    /// editor, y solo al cerrarlo salía. Nadie que pulsa `Ctrl+C` está pidiendo
    /// que se le abra un editor.
    pub fn take_pending_shell(&mut self) -> Option<super::PendingShell> {
        let pendiente = self.pending_shell.take();
        if self.quit { None } else { pendiente }
    }

    /// Abre el modal de crear fichero vacío (Shift+F4, #290).
    ///
    /// Pide un nombre porque el fichero lo crea el DAEMON (`fs.create`) y no
    /// el editor: así la creación pasa por la política y por el journal, con
    /// su undo, como cualquier otra mutación (regla dura 4). Antes se lanzaba
    /// el editor con un buffer vacío y el fichero aparecía al guardar, fuera
    /// de norte entero.
    pub fn open_edit_new(&mut self) {
        self.modal = Some(Modal::EditNew {
            dir: self.focused().dir().clone(),
            name: String::new(),
            error: None,
        });
    }

    /// Abre el modal de «guardar el espacio de trabajo como perfil» (#306).
    ///
    /// Prellenado con el perfil ACTIVO si lo hay: lo normal es partir del que
    /// tienes puesto, y así «guardar como» sobre el mismo nombre es guardar
    /// encima — que es lo que hace cualquier programa. Sin perfil, vacío: no
    /// hay un nombre por defecto que no sea una invención.
    pub fn open_profile_save_as(&mut self) {
        let name = self
            .active_profile
            .as_ref()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.modal = Some(Modal::ProfileSaveAs { name, error: None });
    }

    /// Cierra el modal de perfil tras un guardado que SÍ escribió.
    pub fn prompt_submitted_profile_save(&mut self) {
        self.prompt_submitted(PromptKind::ProfileSaveAs);
    }

    /// Deja el diagnóstico bajo el campo; el nombre sobrevive.
    pub fn prompt_error_profile_save(&mut self, msg: String) {
        self.prompt_set_error(PromptKind::ProfileSaveAs, msg);
    }

    /// Valida el nombre y devuelve el DESTINO completo. Mismo contrato que
    /// [`Self::mkdir_confirm`], incluido el de NO cerrar el modal: lo cierra
    /// [`Self::edit_new_submitted`] cuando la task ya encoló.
    ///
    /// El directorio sale del MODAL, no del pane con foco: se ató al abrirlo
    /// (ver [`Modal::EditNew`]).
    ///
    /// La guarda de localidad se repite sobre ese directorio atado. Con el
    /// `dir` en el modal es DEFENSIVA —el despacho ya la hizo y nadie puede
    /// cambiar ese valor—, y se queda porque lo que la haría necesaria es
    /// exactamente el cambio que a alguien le parecerá una simplificación:
    /// volver a leer `focused().dir()` aquí. Entonces un pane que se fuera a
    /// `sftp://` entre abrir y confirmar crearía un fichero que ningún editor
    /// de esta máquina puede abrir después.
    pub fn edit_new_confirm(&mut self) -> Option<VPath> {
        let Some(Modal::EditNew { dir, name, .. }) = &self.modal else {
            return None;
        };
        if norte_vfs_local::vpath_to_native(dir).is_err() {
            // El MISMO mensaje que el shell y el editor: nombra la ubicación
            // saneada en vez de decir «no» a secas.
            let msg = crate::gestures::shell_remote_message(self);
            self.edit_new_set_error(msg);
            return None;
        }
        let destino = match norte_proto::Segment::new(name.as_bytes().to_vec()) {
            Ok(seg) => dir.join(seg),
            Err(e) => {
                let msg = e.to_string();
                self.edit_new_set_error(msg);
                return None;
            }
        };
        Some(destino)
    }

    /// Cierra el modal tras un submit que SÍ encoló.
    pub fn edit_new_submitted(&mut self) {
        self.prompt_submitted(PromptKind::EditNew);
    }

    /// Deja el diagnóstico de un submit fallido; el nombre sobrevive.
    pub fn edit_new_set_error(&mut self, msg: String) {
        self.prompt_set_error(PromptKind::EditNew, msg);
    }

    /// Abre el prompt de destino de una transferencia ([`Modal::TransferDest`]).
    ///
    /// Prellenado con la dirección del panel con foco, en forma wire: es la
    /// que [`Self::transfer_dest_confirm`] sabe volver a leer, y editarle la
    /// cola es más corto que teclearla entera. No-op si no hay nada que
    /// transferir: jamás un diálogo sobre un lote vacío.
    pub fn open_transfer_dest(&mut self, kind: TransferKind) {
        if self.focused().marked_paths().is_empty() {
            return;
        }
        self.modal = Some(Modal::TransferDest {
            kind,
            input: self.focused().dir().to_wire(),
            error: None,
        });
    }

    /// Añade un carácter al destino en curso. No-op sin su modal. Mismo tope
    /// que el resto de los prompts de texto libre.
    pub fn transfer_dest_push(&mut self, c: char) {
        self.prompt_push(PromptKind::TransferDest, c);
    }

    /// Borra el último CARÁCTER del destino, escape porcentual incluido.
    /// No-op sin su modal.
    ///
    /// `String::pop` borraba un carácter del TEXTO, y el texto es forma wire:
    /// retroceder sobre `%C3%A9` dejaba `%C3%A`, que ya no parsea
    /// (`BadEscape`) — una pulsación no borraba una letra del nombre, corrompía
    /// un escape (#246 M3).
    pub fn transfer_dest_pop(&mut self) {
        self.prompt_pop(PromptKind::TransferDest);
    }

    /// Cancela el prompt de destino sin transferir nada.
    pub fn cancel_transfer_dest(&mut self) {
        self.cancel_prompt(PromptKind::TransferDest);
    }

    /// Lee el destino tecleado y ABRE la transferencia por la puerta de
    /// siempre ([`Self::open_transfer_to_dir`]).
    ///
    /// Una dirección que no parsea deja su diagnóstico en el propio modal y
    /// conserva lo tecleado, como el resto de los prompts. Devuelve `true` si
    /// se pasó al modal siguiente.
    pub fn transfer_dest_confirm(&mut self) -> bool {
        let Some(Modal::TransferDest { kind, input, .. }) = &self.modal else {
            return false;
        };
        let (kind, input) = (*kind, input.clone());
        // Se lee la forma WIRE y nada más: un texto que parece una ruta local
        // (`/home/…`) no es una dirección de norte, y adivinarle un scheme es
        // como una copia acaba en otro backend del que el lector creía.
        match VPath::parse(&input) {
            Ok(dir) if dir == *self.focused().dir() => {
                // El prompt se PRELLENA con el directorio de origen, así que
                // un `Enter` sin editar pedía copiar cada marca sobre sí
                // misma: `ops::copy_task` lo rechaza, pero una a una, y el
                // lector se encontraba N tareas fallidas en vez de una línea
                // en el propio diálogo (#244 m6).
                if let Some(Modal::TransferDest { error, .. }) = &mut self.modal {
                    *error = Some(t("msg-transfer-dest-same"));
                }
                false
            }
            Ok(dir) => {
                self.open_transfer_to_dir(kind, self.focus(), dir, None);
                true
            }
            Err(e) => {
                if let Some(Modal::TransferDest { error, .. }) = &mut self.modal {
                    *error = Some(ta("msg-transfer-dest-invalid", &[("err", &e.to_string())]));
                }
                false
            }
        }
    }

    /// Abre el prompt de `pane.command-line` (#135).
    pub fn open_command_line(&mut self) {
        self.modal = Some(Modal::CommandLine {
            command: String::new(),
            error: None,
        });
    }

    /// Añade un carácter a la línea de comandos. No-op sin su modal. Mismo
    /// tope en `chars` que el resto de los prompts de texto libre.
    /// Alcanzar el tope DEJA DIAGNÓSTICO, a diferencia del resto de los
    /// prompts de texto libre (review de S4, M4). Un nombre de directorio
    /// truncado falla al crearse y se ve; una línea de comandos truncada
    /// CORRE — `rm -rf /proyecto-viejo` recortado a `rm -rf /proyecto` es una
    /// orden distinta, no journaleada y no deshacible. Callarse el recorte
    /// aquí es dejar pulsar Enter a ciegas.
    pub fn command_line_push(&mut self, c: char) {
        self.prompt_push(PromptKind::CommandLine, c);
    }

    /// Borra el último carácter de la línea. No-op sin su modal.
    pub fn command_line_pop(&mut self) {
        self.prompt_pop(PromptKind::CommandLine);
    }

    /// Cancela `Modal::CommandLine` sin ejecutar nada (mismo contrato y guard
    /// que [`Self::cancel_mkdir`]: un modal de DECISIÓN jamás se cierra por
    /// aquí).
    pub fn cancel_command_line(&mut self) {
        self.cancel_prompt(PromptKind::CommandLine);
    }

    /// Valida y devuelve la línea; NO cierra el modal — el caller cierra con
    /// [`Self::command_line_submitted`] tras dejar la suspensión pendiente
    /// (misma disciplina que [`Self::ai_rename_confirm`]).
    ///
    /// La línea se devuelve TAL CUAL, sin `trim`: solo se usa el recortado
    /// para decidir si está vacía. Un comando que empieza por espacio es una
    /// convención real de bash/zsh (`HISTCONTROL=ignorespace`), y recortarlo
    /// cambiaría en silencio lo que el usuario escribió.
    pub fn command_line_confirm(&mut self) -> Option<String> {
        if let Some(Modal::CommandLine { command, error }) = &mut self.modal {
            if command.trim().is_empty() {
                *error = Some(t("modal-command-line-empty"));
                return None;
            }
            return Some(command.clone());
        }
        None
    }

    /// Cierra el prompt tras dejar la suspensión encolada (misma disciplina
    /// de cierre que [`Self::ai_rename_submitted`]).
    pub fn command_line_submitted(&mut self) {
        self.prompt_submitted(PromptKind::CommandLine);
    }

    /// Abre el prompt de la PLANTILLA del renombrado en lote (#310),
    /// prellenado con `[N].[E]` — el nombre tal y como está.
    ///
    /// Prellenar con la identidad y no en blanco: así lo primero que se ve es
    /// la forma que tiene una plantilla, y editarla es más corto que
    /// escribirla entera. Un plan de identidad no renombra nada (los pares que
    /// no cambian se descartan), así que confirmar sin tocar nada es inocuo.
    pub fn open_rename_batch(&mut self) {
        self.modal = Some(Modal::RenameBatchPattern {
            pattern: "[N].[E]".to_owned(),
            error: None,
        });
    }

    /// Valida la plantilla contra los nombres que va a tocar y la devuelve;
    /// NO cierra el modal — el caller cierra con
    /// [`Self::rename_batch_submitted`] tras spawnear la petición de plan,
    /// misma disciplina que [`Self::ai_rename_confirm`].
    ///
    /// Una plantilla que no sirve deja su diagnóstico aquí mismo, bajo el
    /// campo, y devuelve `None`: se explica con el humano delante y antes de
    /// pedirle nada al core.
    pub fn rename_batch_confirm(&mut self) -> Option<String> {
        let names = self.rename_batch_names();
        if let Some(Modal::RenameBatchPattern { pattern, error }) = &mut self.modal {
            let texto = pattern.trim().to_owned();
            return match norte_frontend::rename_pattern::check(&texto, &names) {
                Ok(()) => Some(texto),
                Err(e) => {
                    *error = Some(t(norte_frontend::rename_pattern::error_key(e)));
                    None
                }
            };
        }
        None
    }

    /// Los nombres sobre los que actúa el lote: los MARCADOS, y si no hay
    /// ninguno el del cursor — el mismo operando que cualquier otra operación
    /// (`marked_paths`), y por eso no hay una regla nueva que aprender.
    ///
    /// Solo los que son texto: un par del plan viaja UTF-8 por protocolo, así
    /// que un nombre que no lo sea no puede entrar en un lote (tampoco por el
    /// camino de la IA). Se apartan aquí y el caller lo dice, en vez de
    /// mandarlos y que el plan salga inválido sin explicar cuál sobraba.
    #[must_use]
    pub fn rename_batch_names(&self) -> Vec<String> {
        self.focused()
            .marked_paths()
            .iter()
            .filter_map(|p| {
                p.file_name()
                    .and_then(|s| std::str::from_utf8(s.as_bytes()).ok())
                    .map(std::borrow::ToOwned::to_owned)
            })
            .collect()
    }

    /// Cierra el prompt de la plantilla tras spawnear la petición de plan.
    pub fn rename_batch_submitted(&mut self) {
        self.prompt_submitted(PromptKind::RenameBatch);
    }

    /// Añade un carácter a la plantilla. No-op sin su modal.
    pub fn rename_batch_push(&mut self, c: char) {
        self.prompt_push(PromptKind::RenameBatch, c);
    }

    /// Borra el último carácter de la plantilla. No-op sin su modal.
    pub fn rename_batch_pop(&mut self) {
        self.prompt_pop(PromptKind::RenameBatch);
    }

    /// Cancela el prompt de la plantilla sin lanzar nada.
    pub fn cancel_rename_batch(&mut self) {
        self.cancel_prompt(PromptKind::RenameBatch);
    }

    /// Abre el prompt de instrucción del rename IA (M4-IA).
    pub fn open_ai_rename(&mut self) {
        self.modal = Some(Modal::AiRenameInstruction {
            instruction: String::new(),
            error: None,
        });
    }

    /// Añade un carácter a la instrucción en curso. No-op sin su modal.
    /// Tope en `chars` como el patrón (#103): un paste accidental no
    /// desborda el modal; el límite REAL (4 KiB) lo pone el daemon.
    pub fn ai_rename_push(&mut self, c: char) {
        self.prompt_push(PromptKind::AiRename, c);
    }

    /// Borra el último carácter de la instrucción. No-op sin su modal.
    pub fn ai_rename_pop(&mut self) {
        self.prompt_pop(PromptKind::AiRename);
    }

    /// Cancela `Modal::AiRenameInstruction` sin lanzar nada — el Esc de ESTE
    /// modal de texto libre (mismo contrato y guard que
    /// [`Self::cancel_mkdir`]: un modal de DECISIÓN jamás se cierra por
    /// aquí).
    pub fn cancel_ai_rename(&mut self) {
        self.cancel_prompt(PromptKind::AiRename);
    }

    /// Valida y devuelve la instrucción; NO cierra el modal — el caller
    /// cierra con [`Self::ai_rename_submitted`] tras SPAWNEAR la petición
    /// (audit INFO-7: el spawn en sí no falla; los fallos del modelo llegan
    /// ASÍNCRONOS y salen por la barra, `msg-ai-rename-failed`, no por el
    /// modal). Una instrucción vacía deja su diagnóstico aquí mismo y
    /// devuelve `None`.
    pub fn ai_rename_confirm(&mut self) -> Option<String> {
        if let Some(Modal::AiRenameInstruction { instruction, error }) = &mut self.modal {
            let text = instruction.trim();
            if text.is_empty() {
                *error = Some(t("modal-ai-rename-empty-instruction"));
                return None;
            }
            return Some(text.to_owned());
        }
        None
    }

    /// Cierra el prompt tras un lanzamiento que SÍ salió (M4-IA): misma
    /// disciplina de cierre que [`Self::mkdir_submitted`] (jamás dejar una
    /// pendiente esperando).
    pub fn ai_rename_submitted(&mut self) {
        self.prompt_submitted(PromptKind::AiRename);
    }

    /// Deja un diagnóstico bajo el campo con el texto CONSERVADO. Audit
    /// INFO-7: en el flujo real solo cubre diagnósticos SÍNCRONOS previos al
    /// spawn (hoy, la instrucción vacía la marca el propio
    /// [`Self::ai_rename_confirm`]); un fallo del modelo llega ASYNC con el
    /// prompt ya cerrado y va a la barra, jamás por aquí.
    pub fn ai_rename_set_error(&mut self, msg: String) {
        self.prompt_set_error(PromptKind::AiRename, msg);
    }

    /// Desplaza la ventana del plan IA (audit MAJOR-3): `down` avanza una
    /// pareja, si no retrocede; clampado a `[0, len - ventana]`. No-op sin
    /// su modal. El scroll JAMÁS confirma ni cancela — `dialog_action`
    /// devuelve `None` para `dialog.up`/`dialog.down` en este modal (fuera
    /// de su allowlist de decisión) y el run loop enruta esos comandos aquí.
    pub fn ai_plan_scroll(&mut self, down: bool) {
        if let Some(Modal::AiRenamePlan {
            entries, offset, ..
        }) = &mut self.modal
        {
            let max = entries.len().saturating_sub(AI_RENAME_PAIR_LIMIT);
            *offset = if down {
                (*offset + 1).min(max)
            } else {
                offset.saturating_sub(1)
            };
        }
    }

    /// Desplaza la ventana del modal de sumas (#311), con el mismo clamp que
    /// el del plan IA y por la misma razón: la lista se recorre ENTERA, y el
    /// veredicto que importa —el que no cuadra— puede estar en cualquier
    /// fila. No-op sin su modal.
    pub fn checksums_scroll(&mut self, down: bool) {
        if let Some(Modal::Checksums { rows, offset, .. }) = &mut self.modal {
            let max = rows.len().saturating_sub(AI_RENAME_PAIR_LIMIT);
            *offset = if down {
                (*offset + 1).min(max)
            } else {
                offset.saturating_sub(1)
            };
        }
    }

    /// Deja el plan del LOTE (§17) en el modal del plan IA que lo estaba
    /// esperando. Devuelve `false` si no había ninguno —el humano ya cerró el
    /// modal, o el plan está RETENIDO tras otro modal y lo rellena el run
    /// loop—, para que el caller sepa que tiene que buscarlo en su stash.
    ///
    /// Solo rellena un modal en [`norte_frontend::BatchPlan::Pending`]: una
    /// respuesta jamás pisa a un plan ya resuelto.
    pub fn settle_ai_batch_plan(&mut self, resuelto: &norte_frontend::BatchPlan) -> bool {
        if let Some(Modal::AiRenamePlan { plan, .. }) = &mut self.modal
            && *plan == norte_frontend::BatchPlan::Pending
        {
            *plan = resuelto.clone();
            return true;
        }
        false
    }

    /// Abre el prompt de consulta de la búsqueda semántica (M4-IA-2).
    pub fn open_semantic_search(&mut self) {
        self.modal = Some(Modal::SemanticQuery {
            query: String::new(),
            error: None,
        });
    }

    /// Añade un carácter a la consulta en curso. No-op sin su modal.
    /// Mismo tope en `chars` que la instrucción IA: un paste accidental no
    /// desborda el modal; el límite REAL lo pone el daemon.
    pub fn semantic_push(&mut self, c: char) {
        self.prompt_push(PromptKind::Semantic, c);
    }

    /// Borra el último carácter de la consulta. No-op sin su modal.
    pub fn semantic_pop(&mut self) {
        self.prompt_pop(PromptKind::Semantic);
    }

    /// Cancela `Modal::SemanticQuery` sin lanzar nada — el Esc de ESTE modal
    /// de texto libre (mismo contrato y guard que [`Self::cancel_ai_rename`]:
    /// un modal de DECISIÓN jamás se cierra por aquí).
    pub fn cancel_semantic(&mut self) {
        self.cancel_prompt(PromptKind::Semantic);
    }

    /// Valida y devuelve la consulta; NO cierra el modal — el caller cierra
    /// con [`Self::semantic_submitted`] tras SPAWNEAR la petición (mismo
    /// contrato que [`Self::ai_rename_confirm`]: los fallos del modelo llegan
    /// ASÍNCRONOS y salen por la barra, `msg-semantic-failed`, no por el
    /// modal). Una consulta vacía deja su diagnóstico aquí mismo y devuelve
    /// `None`.
    pub fn semantic_confirm(&mut self) -> Option<String> {
        if let Some(Modal::SemanticQuery { query, error }) = &mut self.modal {
            let text = query.trim();
            if text.is_empty() {
                *error = Some(t("modal-semantic-empty-query"));
                return None;
            }
            return Some(text.to_owned());
        }
        None
    }

    /// Cierra el prompt tras un lanzamiento que SÍ salió (M4-IA-2): misma
    /// disciplina de cierre que [`Self::ai_rename_submitted`] (jamás dejar
    /// una pendiente esperando).
    pub fn semantic_submitted(&mut self) {
        self.prompt_submitted(PromptKind::Semantic);
    }

    /// Deja un diagnóstico bajo el campo con el texto CONSERVADO. Como
    /// [`Self::ai_rename_set_error`]: solo cubre diagnósticos SÍNCRONOS
    /// previos al spawn (hoy, la consulta vacía la marca el propio
    /// [`Self::semantic_confirm`]); un fallo del modelo llega ASYNC con el
    /// prompt ya cerrado y va a la barra, jamás por aquí.
    pub fn semantic_set_error(&mut self, msg: String) {
        self.prompt_set_error(PromptKind::Semantic, msg);
    }

    /// Mueve el cursor de hits (`down` = true baja); la ventana sigue al
    /// cursor, clampada en ambos extremos. No-op sin su modal. El scroll
    /// JAMÁS confirma ni cancela — `dialog_action` devuelve `None` para
    /// `dialog.up`/`dialog.down` en este modal (fuera de su allowlist de
    /// decisión) y el run loop enruta esos comandos aquí (molde
    /// [`Self::ai_plan_scroll`]).
    pub fn semantic_cursor(&mut self, down: bool) {
        if let Some(Modal::SemanticHits {
            hits,
            offset,
            cursor,
        }) = &mut self.modal
        {
            if hits.is_empty() {
                return;
            }
            *cursor = if down {
                (*cursor + 1).min(hits.len() - 1)
            } else {
                cursor.saturating_sub(1)
            };
            if *cursor < *offset {
                *offset = *cursor;
            }
            if *cursor >= *offset + SEMANTIC_HIT_LIMIT {
                *offset = *cursor + 1 - SEMANTIC_HIT_LIMIT;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::app::pane::Pane;
    use crate::app::testutil::*;
    use norte_proto::{Entry, EntryKind, VPath};

    /// #132: la sugerencia del diálogo de empaquetar puede salir con pérdidas
    /// —el nombre del origen no siempre es UTF-8—, y confirmarla tal cual
    /// crearía un fichero con el carácter de reemplazo dentro.
    ///
    /// Dos nombres distintos que no se pueden leer dan la MISMA sugerencia, así
    /// que el segundo empaquetado chocaría contra el archivo del primero. Es el
    /// mismo rechazo, y la misma clave, que el prompt de renombrar.
    #[test]
    fn empaquetar_rehusa_un_nombre_con_el_caracter_de_reemplazo() {
        let mut app = app_dos_panes();
        app.modal = Some(Modal::Pack {
            name: "caf\u{FFFD}.zip".to_owned(),
            error: None,
        });
        assert!(app.pack_confirm().is_none(), "no se empaqueta con eso");
        let Some(Modal::Pack { error, .. }) = &app.modal else {
            panic!("el diálogo sigue abierto para corregirlo");
        };
        assert_eq!(error.as_deref(), Some(t("msg-transfer-name-fffd").as_str()));
    }

    /// Y una extensión que norte no sabe ESCRIBIR se dice en el diálogo, en vez
    /// de empaquetar un zip con nombre de rar.
    #[test]
    fn empaquetar_rehusa_una_extension_que_no_se_escribe() {
        let mut app = app_dos_panes();
        app.modal = Some(Modal::Pack {
            name: "cosas.rar".to_owned(),
            error: None,
        });
        assert!(app.pack_confirm().is_none());
        let Some(Modal::Pack { error, .. }) = &app.modal else {
            panic!("sigue abierto");
        };
        assert!(error.is_some(), "y dice por qué");
    }

    /// #105: F5 de UN ítem abre el nombre editable prefijado con el nombre
    /// ORIGINAL. Sin tocar, el confirm usa los BYTES crudos (regla 1: un
    /// nombre no-UTF8 copiado sin editar jamás pasa por el lossy).
    #[test]
    fn transfer_name_sin_editar_conserva_los_bytes_originales() {
        let dir = VPath::parse("mem:///").unwrap();
        let hostile = dir
            .clone()
            .join(norte_proto::Segment::new(b"informe\xFF\xFE.dat".to_vec()).unwrap());
        let mut app = App::new(
            Pane::new(
                dir.clone(),
                vec![Entry {
                    attrs: std::collections::BTreeMap::new(),
                    path: hostile.clone(),
                    kind: EntryKind::File,
                    size: None,
                    mtime_ms: None,
                }],
            ),
            Pane::new(VPath::parse("mem:///dst").unwrap(), Vec::new()),
        );
        app.open_transfer(TransferKind::Copy, 0, 1, None);
        let (kind, from, dest) = app.transfer_name_confirm().expect("válido");
        assert_eq!(kind, TransferKind::Copy);
        assert_eq!(from, hostile);
        assert_eq!(
            dest,
            VPath::parse("mem:///dst")
                .unwrap()
                .join(norte_proto::Segment::new(b"informe\xFF\xFE.dat".to_vec()).unwrap()),
            "bytes crudos al destino, jamás la forma lossy"
        );
    }

    /// #105: editar sustituye el nombre por el TEXTO tecleado; y un texto
    /// que aún contiene U+FFFD (residuo del prefill lossy de un nombre
    /// hostil) se RECHAZA — confirmarlo escribiría mojibake en disco.
    #[test]
    fn transfer_name_editado_usa_el_texto_y_rechaza_fffd() {
        let dir = VPath::parse("mem:///").unwrap();
        let hostile = dir
            .clone()
            .join(norte_proto::Segment::new(b"x\xFF.dat".to_vec()).unwrap());
        let mut app = App::new(
            Pane::new(
                dir.clone(),
                vec![Entry {
                    attrs: std::collections::BTreeMap::new(),
                    path: hostile,
                    kind: EntryKind::File,
                    size: None,
                    mtime_ms: None,
                }],
            ),
            Pane::new(VPath::parse("mem:///dst").unwrap(), Vec::new()),
        );
        app.open_transfer(TransferKind::Copy, 0, 1, None);
        // Tocar el campo (borra el último char del prefill lossy): el texto
        // sigue llevando el U+FFFD del prefill → rechazo con diagnóstico.
        app.transfer_name_pop();
        assert!(app.transfer_name_confirm().is_none());
        assert!(matches!(
            &app.modal,
            Some(Modal::TransferName { error: Some(_), .. })
        ));
        // Reescrito limpio: vale, y son los bytes del texto.
        while matches!(&app.modal, Some(Modal::TransferName { name, .. }) if !name.is_empty()) {
            app.transfer_name_pop();
        }
        for c in "limpio.dat".chars() {
            app.transfer_name_push(c);
        }
        let (_, _, dest) = app.transfer_name_confirm().expect("limpio");
        assert_eq!(dest, VPath::parse("mem:///dst/limpio.dat").unwrap());
    }

    /// #105: shift+F6 — rename in situ: destino = MISMO dir; confirmar sin
    /// cambiar el nombre es error (no-op), y un nombre nuevo construye el
    /// destino en el propio dir.
    #[test]
    fn rename_construye_en_el_mismo_dir_y_rechaza_el_mismo_nombre() {
        let mut app = app_with_entries(&["a.txt"]);
        app.open_rename();
        assert!(
            app.transfer_name_confirm().is_none(),
            "mismo nombre = no-op, jamás un submit"
        );
        assert!(matches!(
            &app.modal,
            Some(Modal::TransferName { error: Some(_), .. })
        ));
        app.transfer_name_push('2'); // "a.txt2"
        let (kind, from, dest) = app.transfer_name_confirm().expect("nombre nuevo");
        assert_eq!(kind, TransferKind::Move);
        assert_eq!(from, VPath::parse("mem:///a.txt").unwrap());
        assert_eq!(dest, VPath::parse("mem:///a.txt2").unwrap());
    }

    /// #105 (regla 1, corpus canónico): renombrar un nombre hostil a uno
    /// limpio conserva el `from` BYTE-EXACTO para cada nombre del corpus —
    /// el origen jamás pasa por texto, solo el nombre nuevo es tecleado.
    #[test]
    fn rename_de_cada_nombre_hostil_del_corpus_conserva_el_from() {
        let dir = VPath::parse("mem:///").unwrap();
        for (i, hostile) in norte_testkit::corpus::hostile_names().iter().enumerate() {
            let from = dir
                .clone()
                .join(norte_proto::Segment::new(hostile.bytes.clone()).unwrap());
            let mut app = App::new(
                Pane::new(
                    dir.clone(),
                    vec![Entry {
                        attrs: std::collections::BTreeMap::new(),
                        path: from.clone(),
                        kind: EntryKind::File,
                        size: None,
                        mtime_ms: None,
                    }],
                ),
                Pane::new(dir.clone(), Vec::new()),
            );
            app.open_rename();
            while matches!(&app.modal, Some(Modal::TransferName { name, .. }) if !name.is_empty()) {
                app.transfer_name_pop();
            }
            for c in "limpio".chars() {
                app.transfer_name_push(c);
            }
            let (_, got_from, dest) = app
                .transfer_name_confirm()
                .unwrap_or_else(|| panic!("corpus[{i}] {}", hostile.id));
            assert_eq!(got_from, from, "corpus[{i}]: from byte-exacto");
            assert_eq!(dest, VPath::parse("mem:///limpio").unwrap());
        }
    }

    /// #104: el modal de F7 valida con las reglas del `VPath` y devuelve el
    /// destino completo; inválido = diagnóstico en el modal, jamás submit.
    #[test]
    fn el_modal_mkdir_valida_y_construye_el_destino() {
        let mut app = app_with_entries(&["a"]);
        app.open_mkdir();
        for c in "docs".chars() {
            app.mkdir_push(c);
        }
        let target = app.mkdir_confirm().expect("nombre válido");
        assert_eq!(target, VPath::parse("mem:///docs").unwrap());
        assert!(
            app.modal.is_some(),
            "confirmar NO cierra: cierra el submit que encoló (MINOR-1)"
        );
        // Un submit fallido deja el diagnóstico y conserva el nombre…
        app.mkdir_set_error("policy".into());
        assert!(matches!(
            &app.modal,
            Some(Modal::Mkdir { error: Some(_), name }) if name == "docs"
        ));
        // …y el que encoló, cierra.
        app.mkdir_submitted();
        assert!(app.modal.is_none(), "submitted cierra el modal");

        // Vacío: error, modal abierto.
        app.open_mkdir();
        assert!(app.mkdir_confirm().is_none());
        assert!(
            matches!(&app.modal, Some(Modal::Mkdir { error: Some(_), .. })),
            "el diagnóstico queda en el modal"
        );

        // `..` es DotSegment: jamás un destino.
        app.mkdir_push('.');
        app.mkdir_push('.');
        assert!(app.mkdir_confirm().is_none());
        assert!(matches!(
            &app.modal,
            Some(Modal::Mkdir { error: Some(_), .. })
        ));

        // `/` embebido: InvalidByte.
        app.cancel_mkdir();
        app.open_mkdir();
        for c in "a/b".chars() {
            app.mkdir_push(c);
        }
        assert!(app.mkdir_confirm().is_none());

        // Cancelar cierra sin nada.
        app.cancel_mkdir();
        assert!(app.modal.is_none());
    }

    /// #290: `pane.edit-new` PIDE un nombre, porque el fichero lo crea el
    /// daemon y no el editor. Mismo contrato que el de F7 —valida con las
    /// reglas del `VPath`, no cierra al confirmar, conserva lo tecleado tras
    /// un submit fallido— sobre la otra clase de nodo.
    /// El pane es `file://` a propósito: crear un fichero para editarlo se
    /// rehúsa donde no hay forma nativa, y `mem://` no la tiene.
    fn app_local_para_crear() -> App {
        let d = VPath::parse("file:///tmp").expect("wire");
        App::new(
            super::super::Pane::new(d.clone(), Vec::new()),
            super::super::Pane::new(d, Vec::new()),
        )
    }

    #[test]
    fn el_modal_de_fichero_nuevo_valida_y_construye_el_destino() {
        let mut app = app_local_para_crear();
        app.open_edit_new();
        for c in "notas.txt".chars() {
            app.prompt_push(PromptKind::EditNew, c);
        }
        let target = app.edit_new_confirm().expect("nombre válido");
        assert_eq!(target, VPath::parse("file:///tmp/notas.txt").unwrap());
        assert!(app.modal.is_some(), "confirmar NO cierra: cierra el submit");

        // Un submit fallido —política, journal— conserva el nombre.
        app.edit_new_set_error("policy".into());
        assert!(matches!(
            &app.modal,
            Some(Modal::EditNew { error: Some(_), name, .. }) if name == "notas.txt"
        ));
        app.edit_new_submitted();
        assert!(app.modal.is_none(), "submitted cierra el modal");

        // Y los nombres que jamás son un destino siguen sin serlo aquí.
        for malo in ["", "..", "a/b"] {
            app.open_edit_new();
            for c in malo.chars() {
                app.prompt_push(PromptKind::EditNew, c);
            }
            assert!(app.edit_new_confirm().is_none(), "{malo} no es un nombre");
            assert!(
                matches!(&app.modal, Some(Modal::EditNew { error: Some(_), .. })),
                "{malo}: el diagnóstico queda en el modal"
            );
            app.cancel_prompt(PromptKind::EditNew);
        }
    }

    /// El directorio se ATA al abrir el modal, como en la ventana: si el pane
    /// se va a otro sitio entre abrirlo y confirmarlo, el fichero se crea donde
    /// el lector estaba mirando cuando tecleó el nombre.
    #[test]
    fn el_fichero_nuevo_se_crea_donde_se_abrio_el_dialogo() {
        let mut app = app_local_para_crear();
        app.open_edit_new();
        for c in "notas.txt".chars() {
            app.prompt_push(PromptKind::EditNew, c);
        }
        // El pane se muda DEBAJO del modal.
        app.panes[0].begin_listing(
            VPath::parse("file:///otro").unwrap(),
            Vec::new(),
            false,
            None,
        );
        assert_eq!(
            app.edit_new_confirm(),
            Some(VPath::parse("file:///tmp/notas.txt").unwrap()),
            "el destino sale del modal, no del pane de ahora"
        );
    }

    /// Un `Ctrl+C` que llega mientras se arma el editor NO abre el editor: el
    /// bucle drena lo pendiente antes de mirar `quit`, así que sin esta guarda
    /// salir de norte pasaba primero por una sesión de edición que nadie pidió.
    #[test]
    fn pedir_salir_tira_el_programa_pendiente() {
        let mut app = app_with_entries(&["a"]);
        app.pending_shell = Some(crate::app::PendingShell {
            argv: vec![std::ffi::OsString::from("vi")],
            cwd: None,
            wait_for_key: false,
            check_regular: None,
        });
        assert!(app.take_pending_shell().is_some(), "sin salir, se lanza");

        app.pending_shell = Some(crate::app::PendingShell {
            argv: vec![std::ffi::OsString::from("vi")],
            cwd: None,
            wait_for_key: false,
            check_regular: None,
        });
        app.quit = true;
        assert!(app.take_pending_shell().is_none(), "al salir, no");
        assert!(
            app.pending_shell.is_none(),
            "y la intención se tira: no reaparece en la vuelta siguiente"
        );
    }

    /// #103 T9: el modal de patrón marca/desmarca y reporta cuántas marcas
    /// cambió — camino feliz (glob válido, matches reales).
    #[test]
    fn the_pattern_modal_marks_and_reports_how_many() {
        let mut app = app_with_entries(&["a.rs", "b.rs", "c.txt"]);
        app.open_mark_pattern(true);
        assert!(matches!(
            app.modal,
            Some(Modal::MarkPattern { mark: true, .. })
        ));
        app.mark_pattern_push('*');
        app.mark_pattern_push('.');
        app.mark_pattern_push('r');
        app.mark_pattern_push('s');
        let changed = app.mark_pattern_confirm().expect("valid glob");
        assert_eq!(changed, 2);
        assert!(app.modal.is_none());
        assert_eq!(app.focused().marks_len(), 2);
    }

    /// Un patrón inválido (glob que no compila) deja el modal ABIERTO con el
    /// diagnóstico — el usuario conserva lo tecleado para corregirlo — y no
    /// marca nada.
    #[test]
    fn an_invalid_pattern_keeps_the_modal_open_and_marks_nothing() {
        let mut app = app_with_entries(&["a.rs"]);
        app.open_mark_pattern(true);
        app.mark_pattern_push('[');
        assert!(app.mark_pattern_confirm().is_err());
        assert!(app.modal.is_some(), "the user keeps their text to fix it");
        assert_eq!(app.focused().marks_len(), 0);
    }
}
