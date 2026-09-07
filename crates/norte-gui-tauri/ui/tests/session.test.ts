// La disciplina de la secuencia (decisión D7), que es lo que separa a un
// renderer que se recupera de uno que pinta un estado inventado.

import { beforeEach, describe, expect, it } from "vitest";

import { Session } from "../src/session";
import { BRIDGE_VERSION } from "../src/types";
import type { BridgeEnvelope, UiUpdate, ViewSnapshot } from "../src/types";
import { golden } from "./fixtures";

function snapshotPayload(): UiUpdate {
  return golden("updates.json")["snapshot"] as UiUpdate;
}

function env(
  sequence: number,
  payload: UiUpdate,
  instance = "host-1",
): BridgeEnvelope<UiUpdate> {
  return { bridge_version: BRIDGE_VERSION, instance_id: instance, sequence, payload };
}

describe("Session", () => {
  let s: Session;
  beforeEach(() => {
    s = new Session();
    s.receive(env(0, snapshotPayload()));
  });

  // El total es la ALTURA del desplazamiento (`total * alto_de_celda`) y el
  // `aria-rowcount`. El drenaje paginado solo manda parches de filas —también
  // el último lote—, así que si el parche no lo trae, el listado se queda con
  // el de la primera página y un directorio grande topa ahí.
  it("un parche de filas pone al día el TOTAL, no solo las filas", () => {
    const antes = s.view()?.slots.find((x) => x.kind === "browser");
    expect(antes?.kind).toBe("browser");
    const out = s.receive(
      env(1, {
        update: "patch",
        base_sequence: 0,
        changes: [
          {
            change: "rows",
            slot_id: 1,
            generation: 4,
            first_visible: 0,
            rows: [],
            total_rows: 5000,
          },
        ],
      }),
    );
    expect(out.kind).toBe("applied");
    const despues = s.view()?.slots.find((x) => x.kind === "browser");
    expect(despues?.kind === "browser" ? despues.total_rows : null).toBe(5000);
  });

  // Una task caduca a los diez segundos y su fila se va: eso desplaza el
  // resto. El cursor del panel de procesos viajaba solo en la foto entera, así
  // que el renderer se quedaba resaltando la fila N —ya otra tarea, o
  // ninguna— mientras la tecla de cancelar actuaba sobre la que el host tiene
  // acotada. Resaltar una y parar otra es la avería, no el retraso.
  it("un parche de tablero mueve también el cursor del panel de procesos", () => {
    const antes = s.view()?.slots.find((x) => x.kind === "processes");
    expect(antes?.kind, "el corpus trae un panel de procesos").toBe("processes");
    const out = s.receive(
      env(1, {
        update: "patch",
        base_sequence: 0,
        changes: [{ change: "tasks", tasks: [], cursor: 1 }],
      }),
    );
    expect(out.kind).toBe("applied");
    const despues = s.view()?.slots.find((x) => x.kind === "processes");
    expect(despues?.kind === "processes" ? despues.cursor : null).toBe(1);
  });

  // El tablero se queda vacío: `null`, no «lo de antes». Un resalte sobre la
  // nada señala una fila que no está, y la tecla de cancelar promete algo que
  // no puede cumplir.
  it("un tablero vacío apaga el resalte del panel de procesos", () => {
    s.receive(
      env(1, {
        update: "patch",
        base_sequence: 0,
        changes: [{ change: "tasks", tasks: [], cursor: 2 }],
      }),
    );
    const out = s.receive(
      env(2, {
        update: "patch",
        base_sequence: 1,
        changes: [{ change: "tasks", tasks: [], cursor: null }],
      }),
    );
    expect(out.kind).toBe("applied");
    const despues = s.view()?.slots.find((x) => x.kind === "processes");
    expect(despues?.kind === "processes" ? despues.cursor : 99).toBeNull();
  });

  // `pane.names-encoding` retranscribe los nombres, y la ruta del propio
  // directorio es un nombre más. Viajando solo en la foto, las filas se
  // repintaban y el título se quedaba con la lectura vieja.
  it("un parche de cabecera repinta la ruta y lo que falta del listado", () => {
    const out = s.receive(
      env(1, {
        update: "patch",
        base_sequence: 0,
        changes: [
          {
            change: "browser_header",
            slot_id: 1,
            path_display: "⟨mem⟩/casa/café",
            path_hostile: false,
            skipped_note: "2 entradas se saltaron",
            hidden_note: "3 ocultas",
            marks: 4,
          },
        ],
      }),
    );
    expect(out.kind).toBe("applied");
    const slot = s.view()?.slots.find((x) => x.kind === "browser");
    expect(slot?.kind === "browser" ? slot.path_display : null).toBe("⟨mem⟩/casa/café");
    expect(slot?.kind === "browser" ? slot.hidden_note : null).toBe("3 ocultas");
    expect(slot?.kind === "browser" ? slot.marks : null).toBe(4);
  });

  it("aplica un parche sobre su base", () => {
    const out = s.receive(
      env(1, {
        update: "patch",
        base_sequence: 0,
        changes: [{ change: "cursor", slot_id: 1, generation: 4, cursor: 2 }],
      }),
    );
    expect(out.kind).toBe("applied");
    expect(s.sequence()).toBe(1);
  });

  it("un hueco en la secuencia NO se rellena a ojo: se pide una foto", () => {
    const out = s.receive(env(5, { update: "patch", base_sequence: 4, changes: [] }));
    expect(out).toEqual({ kind: "gap", expected: 1, got: 5 });
  });

  it("un parche sobre OTRA base no se aplica", () => {
    const out = s.receive(env(1, { update: "patch", base_sequence: 99, changes: [] }));
    expect(out.kind).toBe("gap");
  });

  it("una secuencia ya vista se ignora, no se reaplica", () => {
    const out = s.receive(env(0, snapshotPayload()));
    expect(out).toEqual({ kind: "ignored", why: "old-sequence" });
  });

  it("un mensaje de otra instancia del host no muta nada", () => {
    const out = s.receive(env(1, snapshotPayload(), "otra-vida"));
    expect(out).toEqual({ kind: "ignored", why: "other-instance" });
    expect(s.instanceId()).toBe("host-1");
  });

  it("una versión que no se conoce NO se interpreta a medias", () => {
    const out = s.receive({
      bridge_version: BRIDGE_VERSION + 7,
      instance_id: "host-1",
      sequence: 1,
      payload: snapshotPayload(),
    });
    expect(out).toEqual({ kind: "incompatible", version: BRIDGE_VERSION + 7 });
  });

  it("una foto cierra el hueco venga en la secuencia que venga", () => {
    expect(
      s.receive(env(9, { update: "patch", base_sequence: 8, changes: [] })).kind,
    ).toBe("gap");
    const out = s.receive(env(12, snapshotPayload()));
    expect(out.kind).toBe("applied");
    expect(s.sequence()).toBe(12);
  });

  it("un cambio de disposición mueve el foco al hueco con el papel activo", () => {
    s.receive(
      env(1, {
        update: "patch",
        base_sequence: 0,
        changes: [
          {
            change: "layout",
            tabs: [],
            cells: [120, 40],
            placements: [
              {
                slot_id: 2,
                x: 0,
                y: 0,
                width: 60,
                height: 38,
                role: "active",
                focus_index: 0,
              },
              {
                slot_id: 1,
                x: 60,
                y: 0,
                width: 60,
                height: 38,
                role: "target",
                focus_index: 1,
              },
            ],
          },
        ],
      }),
    );
    const view = s.view() as ViewSnapshot;
    expect(view.focus).toBe(2);
  });

  it("un cursor de otra generación no se aplica: la fila ya no es esa", () => {
    const antes = (s.view() as ViewSnapshot).slots[0];
    expect(antes?.kind).toBe("browser");
    s.receive(
      env(1, {
        update: "patch",
        base_sequence: 0,
        changes: [{ change: "cursor", slot_id: 1, generation: 999, cursor: 0 }],
      }),
    );
    const despues = (s.view() as ViewSnapshot).slots[0];
    if (despues?.kind === "browser" && antes?.kind === "browser") {
      expect(despues.cursor).toBe(antes.cursor);
    }
  });
});

describe("un cambio que no se conoce", () => {
  it("NO avanza la secuencia: pide una foto", () => {
    const s = new Session();
    s.receive({
      bridge_version: BRIDGE_VERSION,
      instance_id: "host-1",
      sequence: 0,
      payload: golden("updates.json")["snapshot"] as UiUpdate,
    });
    const out = s.receive({
      bridge_version: BRIDGE_VERSION,
      instance_id: "host-1",
      sequence: 1,
      payload: {
        update: "patch",
        base_sequence: 0,
        // Un `ViewChange` de un host más nuevo.
        changes: [{ change: "algo_que_no_existe" }],
      } as unknown as UiUpdate,
    });
    expect(out.kind).toBe("gap");
    expect(s.sequence()).toBe(0);
  });
});
