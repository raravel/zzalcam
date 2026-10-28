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
  after_record: "editor" | "save";
  keep_sessions: boolean;
  filename_pattern: string;
  export_scale: number;
  max_size_mb: number | null;
  autostart: boolean;
}

export interface RecStats {
  elapsed_ms: number;
  frames: number;
  est_bytes: number;
  paused: boolean;
}

export interface Toast {
  kind: "encoding" | "saved" | "cancelled" | "failed" | "warning" | "recovery";
  title: string;
  detail: string;
  path: string | null;
}

export function humanSize(b: number): string {
  if (b >= 1 << 20) return `${(b / (1 << 20)).toFixed(1)} MB`;
  if (b >= 1 << 10) return `${Math.round(b / 1024)} KB`;
  return `${b} B`;
}
