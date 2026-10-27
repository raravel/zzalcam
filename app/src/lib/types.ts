export interface Hotkeys {
  record: string;
  pause: string;
  repeat: string;
  cancel: string;
}

export interface Settings {
  language: string;
  fps: number;
  cursor: boolean;
  countdown: boolean;
  format: "gif" | "webp";
  preset: "best" | "balanced" | "smallest";
  output_dir: string;
  hotkeys: Hotkeys;
}

export interface RecStats {
  elapsed_ms: number;
  frames: number;
  est_bytes: number;
  paused: boolean;
}

export interface Toast {
  kind: "encoding" | "saved" | "cancelled" | "failed";
  title: string;
  detail: string;
  path: string | null;
}

export function humanSize(b: number): string {
  if (b >= 1 << 20) return `${(b / (1 << 20)).toFixed(1)} MB`;
  if (b >= 1 << 10) return `${Math.round(b / 1024)} KB`;
  return `${b} B`;
}
