import { invoke } from "@tauri-apps/api/core";
import type { BridgeStatus, Settings, UpdateResult } from "./types";

const inTauri = "__TAURI_INTERNALS__" in window;
const demoStatus: BridgeStatus = { state: "unpaired", base_url: null, bridge_id: null, detail: null, retry_in_ms: null, updated_at: new Date().toISOString() };

export const api = {
  isDesktop: inTauri,
  status: () => inTauri ? invoke<BridgeStatus>("bridge_status") : Promise.resolve(demoStatus),
  settings: () => inTauri ? invoke<Settings>("get_settings") : Promise.resolve({ ssh_auth_sock: "", insecure: false, launch_at_login: false }),
  pair: (payload: string) => invoke<BridgeStatus>("pair_bridge", { payload }),
  start: () => invoke<BridgeStatus>("start_bridge"),
  pause: () => invoke<BridgeStatus>("pause_bridge"),
  saveSettings: (settings: Settings) => invoke<Settings>("save_settings", { settings }),
  checkForUpdates: () => invoke<UpdateResult>("check_for_updates"),
  installUpdate: () => invoke<void>("install_update"),
};
