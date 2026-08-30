//! Guardar el espacio de trabajo actual como un PERFIL (#306, ADR 0079).
//!
//! Es la otra mitad de los perfiles: hasta aquí se podían elegir y cambiar en
//! caliente, pero solo existían los que alguien hubiera escrito a mano.
//!
//! Lo que se guarda es lo que se VE, y esa frase decide el contenido:
//!
//! - la disposición viva, en `profiles/<nombre>/layouts/workspace.toml`, con
//!   el `[ui] layout` del perfil apuntándola;
//! - el directorio de cada hueco en `[profile.start]`, que es lo que hace útil
//!   un perfil en su PRIMER arranque, antes de que tenga estado guardado;
//! - los escalares de `[ui]` que DIFIEREN de la capa del usuario. Solo esos:
//!   copiar los que coinciden llena el fichero de líneas que luego nadie sabe
//!   si son deliberadas o ruido;
//! - el `keymap.toml` del perfil de partida, si lo había. «Guardar como»
//!   produce un perfil que se comporta igual que el que tenías: si cambiaste
//!   atajos, el nuevo los lleva.
//!
//! Lo que NO se guarda: los favoritos, las conexiones y el resto de secciones
//! que no son `[ui]`. Un perfil es un espacio de TRABAJO, no una copia de la
//! configuración entera, y duplicar la hotlist del usuario en cada perfil la
//! congelaría — la del usuario sigue viéndose por debajo.

use norte_i18n::{t, ta};

use crate::app::{App, Modal};

/// Enter en el modal de «guardar como perfil».
///
/// Valida el nombre ANTES de tocar disco —acaba siendo un directorio— y deja
/// el modal abierto con el diagnóstico si no vale: lo tecleado sobrevive para
/// corregirlo, que es la disciplina de los diez prompts.
///
/// El disco va por `spawn_blocking` (regla 2): son tres ficheros con lock y
/// tmp+rename.
pub async fn profile_save_as(app: &mut App) {
    let Some(Modal::ProfileSaveAs { name, .. }) = &app.modal else {
        return;
    };
    let nombre = std::ffi::OsString::from(name.trim());
    if !norte_config::valid_profile_name(&nombre) {
        app.prompt_error_profile_save(t("msg-profile-name-invalid"));
        return;
    }
    let Some(dir) = norte_config::profiles_dir_from(&|k| std::env::var_os(k)) else {
        app.prompt_error_profile_save(t("msg-no-config-dir"));
        return;
    };
    let snap = snapshot_de(app);
    let n = nombre.clone();
    let res =
        tokio::task::spawn_blocking(move || norte_config::save_profile(&dir, &n, &snap)).await;
    match res {
        Ok(Ok(_)) => {
            app.prompt_submitted_profile_save();
            app.message = Some(ta(
                "msg-profile-saved",
                &[(
                    "name",
                    &crate::app::detail_for_bar(&nombre.to_string_lossy()),
                )],
            ));
        }
        Ok(Err(e)) => {
            app.prompt_error_profile_save(ta(
                "msg-profile-save-failed",
                &[("error", &crate::app::io_error_category(&e))],
            ));
        }
        // Un panic al escribir es un bug NUESTRO: que reviente visible, como
        // en el resto de los `persist_*` del binario.
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}

/// Lo que hay en pantalla, en la forma que `norte-config` escribe.
fn snapshot_de(app: &App) -> norte_config::ProfileSnapshot {
    let layout_toml = norte_frontend::layout::config::to_toml(&app.layout).ok();
    let start = app
        .layout
        .slot_ids()
        .into_iter()
        .filter_map(|id| {
            let norte_frontend::layout::SlotId(n) = id;
            let pane = app.panes.browser(id)?;
            Some((n.to_string(), pane.dir().to_wire()))
        })
        .collect();
    norte_config::ProfileSnapshot {
        title: None,
        layout_toml,
        // Los escalares se dejan para cuando exista la edición de config por
        // pantalla: hoy lo que el lector cambia en marcha —tema, preset— ya
        // se persiste por su propio camino, y copiarlo aquí escribiría dos
        // veces lo mismo con dos verdades posibles.
        ui: Vec::new(),
        start,
        keymap: keymap_del_perfil_activo(app),
    }
}

/// El `keymap.toml` del perfil ACTIVO, tal cual, o `None` si no hay perfil o
/// no tiene uno.
///
/// Byte a byte y sin reescribirlo: es un fichero del lector, con sus
/// comentarios.
fn keymap_del_perfil_activo(app: &App) -> Option<Vec<u8>> {
    let dir = app.config_write_dir()?;
    std::fs::read(dir.join("keymap.toml")).ok()
}
