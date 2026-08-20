// El transporte: lo único que sabe que debajo hay un Tauri.
//
// Todo lo demás del renderer habla con `HostPort`, así que un test lo sustituye
// por una tabla y una pantalla completa se puede ejercitar sin abrir ventana
// (ni instalar WebKitGTK).

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import type { ActionAck, BridgeEnvelope, HostCatalog, UiAction, UiUpdate } from "./types";

/** El evento por el que llegan las actualizaciones, en orden. */
export const EVENT_UPDATE = "norte://update";
/** El host avisa de que este suscriptor se quedó atrás. */
export const EVENT_LAGGED = "norte://lagged";

/** Lo que el renderer necesita del host. Cinco cosas y ninguna genérica. */
export interface HostPort {
  initialSnapshot(): Promise<BridgeEnvelope<UiUpdate>>;
  dispatch(action: UiAction): Promise<ActionAck>;
  requestSnapshot(): Promise<ActionAck>;
  catalog(): Promise<HostCatalog>;
  onUpdate(cb: (env: BridgeEnvelope<UiUpdate>) => void): Promise<() => void>;
  onLagged(cb: () => void): Promise<() => void>;
}

// Adrede NO hay un `rpc(method, params)`: la webview no puede pedirle al
// daemon lo que se le ocurra, solo lo que el host expone (decisión D11).
/// Manda las muestras de la 3.6. Rechaza si el binario no lleva la feature.
export function invokeMetrics(what: string, samples: number[]): Promise<void> {
  return invoke<void>("metrics", { sample: { what, samples } });
}

export const tauriPort: HostPort = {
  initialSnapshot: () => invoke<BridgeEnvelope<UiUpdate>>("initial_snapshot"),
  dispatch: (action) => invoke<ActionAck>("dispatch", { action }),
  requestSnapshot: () => invoke<ActionAck>("request_snapshot"),
  catalog: () => invoke<HostCatalog>("catalog"),
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
};
