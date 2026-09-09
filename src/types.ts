export type BridgeState = "unpaired" | "starting" | "connected" | "reconnecting" | "paused" | "needs_pairing" | "agent_unavailable" | "error";
export interface BridgeStatus { state: BridgeState; base_url: string | null; bridge_id: string | null; detail: string | null; retry_in_ms: number | null; updated_at: string; }
export interface Settings { ssh_auth_sock: string; insecure: boolean; launch_at_login: boolean; }
export interface UpdateResult { available: boolean; version: string | null; }
