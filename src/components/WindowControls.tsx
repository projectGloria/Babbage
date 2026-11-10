import { getCurrentWindow } from "@tauri-apps/api/window";
import { Minus, Square, X } from "lucide-react";
import { useTranslation } from "react-i18next";

/** Window buttons for the frameless (launcher) window, which has no native title bar. */
export function WindowControls() {
  const { t } = useTranslation("common");
  const btn = "btn btn-ghost btn-icon h-7 w-7 !rounded-full text-muted";
  return (
    <div className="flex items-center gap-0.5">
      <button type="button" title={t("window.close")} aria-label={t("window.close")}
        onClick={() => getCurrentWindow().close().catch(() => {})} className={btn}>
        <X className="h-3.5 w-3.5" strokeWidth={1.75} />
      </button>
      <button type="button" title={t("window.minimize")} aria-label={t("window.minimize")}
        onClick={() => getCurrentWindow().minimize().catch(() => {})} className={btn}>
        <Minus className="h-3.5 w-3.5" strokeWidth={1.75} />
      </button>
      <button type="button" title={t("window.maximize")} aria-label={t("window.maximize")}
        onClick={() => getCurrentWindow().toggleMaximize().catch(() => {})} className={btn}>
        <Square className="h-3 w-3" strokeWidth={1.75} />
      </button>
    </div>
  );
}
