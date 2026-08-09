//! `norte help` (H3g) end to end: what the command prints, what it exits with,
//! and what it does when the reader walks away mid-page.

use assert_cmd::Command;

fn norte() -> Command {
    let mut c = Command::cargo_bin("norte").expect("binario norte compilado");
    // The page names the reader's OWN keys, so an inherited `~/.config/norte`
    // would make every assertion here depend on whoever runs it.
    c.env("NORTE_CONFIG_DIR", "/nonexistent-norte-config");
    c.env("NORTE_LANG", "en");
    c
}

#[test]
fn help_sin_argumentos_imprime_el_indice() {
    let out = norte().arg("help").output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).expect("UTF-8");
    let index = norte_help::topic(norte_help::Lang::En, "index").expect("index topic");
    assert!(text.starts_with(&index.title), "{text:.80}");
}

#[test]
fn una_pagina_por_id_se_imprime_entera() {
    let out = norte().args(["help", "copying"]).output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).expect("UTF-8");
    let topic = norte_help::topic(norte_help::Lang::En, "copying").expect("corpus topic");
    assert!(text.starts_with(&topic.title));
    // A `{{cmd:}}` mark resolved against the DEFAULT preset (the config dir
    // above does not exist), so the page names a real key.
    assert!(text.contains("F5"), "{text:.400}");
}

#[test]
fn una_pagina_desconocida_falla_con_una_linea_util() {
    let out = norte().args(["help", "no-such-topic"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty(), "an error does not print a page");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--list"), "the error names the way out: {err}");
}

/// The id is echoed back into a terminal, so it is masked and capped on the
/// way — an error line is exactly where a pasted string with an ESC ends up.
#[test]
fn un_id_hostil_no_se_devuelve_en_crudo() {
    let hostile = format!("\u{1b}[31m\u{202e}{}", "x".repeat(200));
    let out = norte().args(["help", &hostile]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!err.contains('\u{1b}'), "ESC survived: {err:?}");
    assert!(!err.contains('\u{202e}'), "bidi override survived: {err:?}");
    assert!(err.chars().count() < 200, "no cap: {}", err.chars().count());
}

#[test]
fn list_enumera_las_paginas_y_search_dice_por_que() {
    let out = norte().args(["help", "--list"]).output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).expect("UTF-8");
    for topic in norte_help::topics(norte_help::Lang::En) {
        assert!(text.contains(topic.id.as_str()), "{} missing", topic.id);
    }

    let out = norte().args(["help", "--search", "copy"]).output().unwrap();
    assert!(out.status.success());
    let hits = String::from_utf8(out.stdout).expect("UTF-8");
    assert!(hits.contains("copying"), "{hits}");
}

#[test]
fn una_busqueda_sin_resultados_sale_1_y_calla() {
    let out = norte()
        .args(["help", "--search", "zzzzz-no-such-word"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "grep's convention");
    assert!(out.stdout.is_empty());
    assert!(!out.stderr.is_empty(), "and says so on stderr");
}

#[test]
fn keys_nombra_las_teclas_del_preset() {
    let out = norte().args(["help", "keys"]).output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).expect("UTF-8");
    assert!(text.contains("F5"), "{text:.200}");
    assert!(
        !text.contains("help-cmd-") && !text.contains("dialog-cmd-"),
        "raw Fluent id in the sheet"
    );
}

/// The page is localized from the same corpus as the apps.
#[test]
fn el_idioma_sale_del_entorno() {
    let out = norte()
        .env("NORTE_LANG", "es")
        .args(["help", "copying"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).expect("UTF-8");
    let es = norte_help::topic(norte_help::Lang::Es, "copying").expect("corpus es");
    assert!(text.starts_with(&es.title), "{text:.80}");
}

#[cfg(unix)]
#[test]
fn cerrar_la_tuberia_no_produce_un_panic() {
    use std::io::Read as _;
    use std::process::{Command as Proc, Stdio};

    // Reading one byte and dropping the pipe is `| head -1`: `println!` would
    // panic on the next write and print a Rust backtrace at the reader.
    let mut child = Proc::new(assert_cmd::cargo::cargo_bin("norte"))
        .args(["help", "--json"])
        .env("NORTE_CONFIG_DIR", "/nonexistent-norte-config")
        .env("NORTE_LANG", "en")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    let mut buf = [0u8; 1];
    let _ = child.stdout.take().expect("stdout").read(&mut buf);
    let out = child.wait_with_output().expect("wait");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !err.contains("panicked"),
        "a closed pipe must be a quiet exit, not a panic: {err}"
    );
}

#[test]
fn el_json_no_diverge_del_golden() {
    let out = norte().args(["help", "--json"]).output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let got: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON válido");
    let pretty = format!(
        "{}\n",
        serde_json::to_string_pretty(&got).expect("re-serializa")
    );
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/goldens/help-en.json");
    if std::env::var_os("NORTE_UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().expect("dir")).expect("mkdir");
        std::fs::write(&path, &pretty).expect("write golden");
        return;
    }
    let want = std::fs::read_to_string(&path)
        .expect("golden — regenéralo con NORTE_UPDATE_GOLDEN=1")
        // A Windows checkout rewrites text files to CRLF; `.gitattributes`
        // pins json to LF and this is the belt.
        .replace("\r\n", "\n");
    assert_eq!(
        pretty, want,
        "the JSON shape moved: regenerate deliberately"
    );
}

#[test]
fn una_pagina_sola_en_json_lleva_sus_filas_resueltas() {
    let out = norte()
        .args(["help", "copying", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON válido");
    assert_eq!(v["version"], 2);
    let topics = v["topics"].as_array().expect("topics");
    assert_eq!(topics.len(), 1);
    assert_eq!(topics[0]["id"], "copying");
    let rows = topics[0]["commands"].as_array().expect("commands");
    assert!(!rows.is_empty());
    assert!(rows[0]["command"].is_string());
    assert!(rows[0]["label"].is_string());
    assert!(
        !topics[0]["text"].as_str().expect("text").is_empty(),
        "the rendered page rides along, which is what an agent reads"
    );
}

/// v2 (K3b): the keyboard page carries STRUCTURED rows, each saying whether
/// this build runs the key. Before it, `--json` gave a consumer only prose,
/// and the prose listed nothing but working keys — which is the reading the
/// version bump exists to invalidate.
#[test]
fn la_pagina_de_teclas_en_json_lleva_filas_con_su_disponibilidad() {
    let out = norte().args(["help", "keys", "--json"]).output().unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON válido");
    assert_eq!(
        v["version"], 2,
        "the shape's meaning changed, not just a field"
    );
    let topics = v["topics"].as_array().expect("topics");
    assert_eq!(topics.len(), 1);
    let keys = topics[0]["keys"].as_array().expect("keys rows");
    assert!(!keys.is_empty());
    for key in keys {
        assert!(key["chord"].is_string(), "{key}");
        assert!(key["command"].is_string(), "{key}");
        assert!(
            ["browse", "viewer", "dialog"].contains(&key["screen"].as_str().expect("screen")),
            "{key}"
        );
        // Always there, and one of the two words this process can honestly
        // say: a row with no availability would be a row a consumer has to
        // guess about, and `here`/`not-here` would be claims about a frontend
        // this command is not.
        assert!(
            ["built", "not-built"].contains(&key["availability"]["state"].as_str().expect("state")),
            "{key}"
        );
    }
    // The prose pages keep the v1 shape and gain no empty array.
    let copying = norte()
        .args(["help", "copying", "--json"])
        .output()
        .unwrap();
    let c: serde_json::Value = serde_json::from_slice(&copying.stdout).expect("JSON válido");
    assert!(
        c["topics"][0].get("keys").is_none(),
        "only the keyboard page has keys"
    );
}

#[test]
fn una_pagina_desconocida_en_json_sigue_siendo_un_error() {
    let out = norte()
        .args(["help", "no-such-topic", "--json"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(1),
        "an empty array would hide the caller's bug"
    );
    assert!(out.stdout.is_empty());
}
