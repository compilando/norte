// The sequence discipline (decision D7), which is what separates a renderer
// that recovers from one that paints a made-up state.

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

  // The total is the scroll HEIGHT (`total * row_height`) and the
  // `aria-rowcount`. Paginated draining only ships row patches — including
  // the last batch — so if the patch doesn't carry it, the listing keeps the
  // first page's total, and a big directory runs into that.
  it("a row patch updates the TOTAL, not just the rows", () => {
    const before = s.view()?.slots.find((x) => x.kind === "browser");
    expect(before?.kind).toBe("browser");
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
            icon_column: false,
            total_rows: 5000,
          },
        ],
      }),
    );
    expect(out.kind).toBe("applied");
    const after = s.view()?.slots.find((x) => x.kind === "browser");
    expect(after?.kind === "browser" ? after.total_rows : null).toBe(5000);
  });

  // A task expires after ten seconds and its row disappears: that shifts the
  // rest. The processes panel's cursor used to travel only in the full
  // snapshot, so the renderer kept highlighting row N — by then another task,
  // or none — while the cancel key acted on the one the host has bounded.
  // Highlighting one and stopping another is the breakage, not the lag.
  it("a board patch also moves the processes panel cursor", () => {
    const before = s.view()?.slots.find((x) => x.kind === "processes");
    expect(before?.kind, "the corpus carries a processes panel").toBe("processes");
    const out = s.receive(
      env(1, {
        update: "patch",
        base_sequence: 0,
        changes: [{ change: "tasks", tasks: [], cursor: 1 }],
      }),
    );
    expect(out.kind).toBe("applied");
    const after = s.view()?.slots.find((x) => x.kind === "processes");
    expect(after?.kind === "processes" ? after.cursor : null).toBe(1);
  });

  // The board goes empty: `null`, not "whatever was there before". A
  // highlight over nothing points at a row that isn't there, and the cancel
  // key promises something it can't deliver.
  it("an empty board turns off the processes panel highlight", () => {
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
    const after = s.view()?.slots.find((x) => x.kind === "processes");
    expect(after?.kind === "processes" ? after.cursor : 99).toBeNull();
  });

  // `pane.names-encoding` re-transcribes the names, and the directory's own
  // path is one more name. Traveling only in the snapshot, the rows got
  // repainted and the title kept the old reading.
  it("a header patch repaints the path and what's missing from the listing", () => {
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
            mark_ruler: [0, 1, 9],
          },
        ],
      }),
    );
    expect(out.kind).toBe("applied");
    const slot = s.view()?.slots.find((x) => x.kind === "browser");
    expect(slot?.kind === "browser" ? slot.path_display : null).toBe("⟨mem⟩/casa/café");
    expect(slot?.kind === "browser" ? slot.hidden_note : null).toBe("3 ocultas");
    expect(slot?.kind === "browser" ? slot.marks : null).toBe(4);
    // The rule travels with the header: marking without moving has to
    // repaint it, or it's left showing the old marks.
    expect(slot?.kind === "browser" ? slot.mark_ruler : null).toEqual([0, 1, 9]);
  });

  it("applies a patch on top of its base", () => {
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

  it("a gap in the sequence is NOT filled by guessing: a snapshot is requested", () => {
    const out = s.receive(env(5, { update: "patch", base_sequence: 4, changes: [] }));
    expect(out).toEqual({ kind: "gap", expected: 1, got: 5 });
  });

  it("a patch on ANOTHER base is not applied", () => {
    const out = s.receive(env(1, { update: "patch", base_sequence: 99, changes: [] }));
    expect(out.kind).toBe("gap");
  });

  it("an already-seen sequence is ignored, not reapplied", () => {
    const out = s.receive(env(0, snapshotPayload()));
    expect(out).toEqual({ kind: "ignored", why: "old-sequence" });
  });

  it("a message from another host instance mutates nothing", () => {
    const out = s.receive(env(1, snapshotPayload(), "another-life"));
    expect(out).toEqual({ kind: "ignored", why: "other-instance" });
    expect(s.instanceId()).toBe("host-1");
  });

  it("an unrecognized version is NOT half-interpreted", () => {
    const out = s.receive({
      bridge_version: BRIDGE_VERSION + 7,
      instance_id: "host-1",
      sequence: 1,
      payload: snapshotPayload(),
    });
    expect(out).toEqual({ kind: "incompatible", version: BRIDGE_VERSION + 7 });
  });

  it("a snapshot closes the gap whatever sequence it arrives on", () => {
    expect(
      s.receive(env(9, { update: "patch", base_sequence: 8, changes: [] })).kind,
    ).toBe("gap");
    const out = s.receive(env(12, snapshotPayload()));
    expect(out.kind).toBe("applied");
    expect(s.sequence()).toBe(12);
  });

  it("a layout change moves focus to the slot with the active role", () => {
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

  it("a cursor from another generation is not applied: that row isn't it anymore", () => {
    const before = (s.view() as ViewSnapshot).slots[0];
    expect(before?.kind).toBe("browser");
    s.receive(
      env(1, {
        update: "patch",
        base_sequence: 0,
        changes: [{ change: "cursor", slot_id: 1, generation: 999, cursor: 0 }],
      }),
    );
    const after = (s.view() as ViewSnapshot).slots[0];
    if (after?.kind === "browser" && before?.kind === "browser") {
      expect(after.cursor).toBe(before.cursor);
    }
  });
});

describe("an unrecognized change", () => {
  it("does NOT advance the sequence: it requests a snapshot", () => {
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
        // A `ViewChange` from a newer host.
        changes: [{ change: "algo_que_no_existe" }],
      } as unknown as UiUpdate,
    });
    expect(out.kind).toBe("gap");
    expect(s.sequence()).toBe(0);
  });
});
