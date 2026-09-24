// The transport: the only thing that knows there is a Tauri underneath.
//
// Everything else in the renderer talks to `HostPort`, so a test can swap it
// for a table and a whole screen can be exercised without opening a window
// (nor installing WebKitGTK).

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import type {
  ActionAck,
  BridgeEnvelope,
  HostCatalog,
  UiAction,
  UiUpdate,
  WindowVerb,
} from "./types";

/** The event updates arrive through, in order. */
export const EVENT_UPDATE = "norte://update";
/** The host warns that this subscriber fell behind. */
export const EVENT_LAGGED = "norte://lagged";
/** The catalogue changed (today: the theme). It has to be asked for again. */
export const EVENT_CATALOG = "norte://catalog";

/** What the renderer needs from the host. Five things and none generic. */
export interface HostPort {
  initialSnapshot(): Promise<BridgeEnvelope<UiUpdate>>;
  dispatch(action: UiAction): Promise<ActionAck>;
  requestSnapshot(): Promise<ActionAck>;
  catalog(): Promise<HostCatalog>;
  /** The bytes of the open image. Empty = there is none. */
  imageBytes(): Promise<ArrayBuffer>;
  /** The window's own title bar (ADR 0136): minimize, maximize, close or
   *  start dragging THIS window. Without it the binary rejects it. */
  windowControl(verb: WindowVerb): Promise<void>;
  onUpdate(cb: (env: BridgeEnvelope<UiUpdate>) => void): Promise<() => void>;
  onLagged(cb: () => void): Promise<() => void>;
  /** The catalogue changed: it has to be asked for again and whatever comes
   *  out of it re-applied. Today only the theme triggers it. */
  onCatalog(cb: () => void): Promise<() => void>;
}

// Deliberately NO `rpc(method, params)`: the webview cannot ask the daemon
// for whatever comes to mind, only what the host exposes (decision D11).
/// Sends 3.6's samples. Rejects if the binary does not carry the feature.
export function invokeMetrics(what: string, samples: number[]): Promise<void> {
  return invoke<void>("metrics", { sample: { what, samples } });
}

export const tauriPort: HostPort = {
  initialSnapshot: () => invoke<BridgeEnvelope<UiUpdate>>("initial_snapshot"),
  dispatch: (action) => invoke<ActionAck>("dispatch", { action }),
  requestSnapshot: () => invoke<ActionAck>("request_snapshot"),
  catalog: () => invoke<HostCatalog>("catalog"),
  // The bytes of the open image, RAW and without a path: the renderer does
  // not name files, it is served the one the host decided to open. Empty =
  // there is none, which the frame already said.
  imageBytes: () => invoke<ArrayBuffer>("image_bytes"),
  // A closed verb and the calling window, nothing else: no window permission
  // in the capability (D11), the door is a binary command.
  windowControl: (verb) => invoke<void>("window_control", { verb }),
  onUpdate: async (cb) => {
    const un = await listen<BridgeEnvelope<UiUpdate>>(EVENT_UPDATE, (e) => {
      cb(e.payload);
    });
    return un;
  },
  onLagged: async (cb) => {
    const un = await listen(EVENT_LAGGED, () => {
      cb();
    });
    return un;
  },
  onCatalog: async (cb) => {
    const un = await listen(EVENT_CATALOG, () => {
      cb();
    });
    return un;
  },
};
