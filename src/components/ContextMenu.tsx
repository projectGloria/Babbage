import { useEffect, useRef } from "react";

export interface ContextMenuItem {
  label: string;
  onSelect: () => void;
  separatorBefore?: boolean;
}

/**
 * Right-click menu for a result. Closes on outside click, Escape, scroll,
 * or after an item runs. Clamped to the viewport so it never clips.
 */
export function ContextMenu({
  x,
  y,
  items,
  onClose,
}: {
  x: number;
  y: number;
  items: ContextMenuItem[];
  onClose: () => void;
}) {
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const prev = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const menuItems = () =>
      Array.from(ref.current?.querySelectorAll<HTMLButtonElement>('[role="menuitem"]') ?? []);
    menuItems()[0]?.focus();

    const closeAndRestore = () => {
      onClose();
      if (prev && document.contains(prev)) prev.focus();
    };
    const onDown = (e: MouseEvent) => {
      if (!ref.current?.contains(e.target as Node)) closeAndRestore();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        closeAndRestore();
        return;
      }
      const list = menuItems();
      if (list.length === 0) return;
      const idx = list.indexOf(document.activeElement as HTMLButtonElement);
      if (e.key === "ArrowDown") {
        e.preventDefault();
        list[(idx + 1) % list.length].focus();
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        list[(idx - 1 + list.length) % list.length].focus();
      } else if (e.key === "Home") {
        e.preventDefault();
        list[0].focus();
      } else if (e.key === "End") {
        e.preventDefault();
        list[list.length - 1].focus();
      }
    };
    const close = () => onClose();
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    window.addEventListener("scroll", close, true);
    window.addEventListener("resize", close);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
      window.removeEventListener("scroll", close, true);
      window.removeEventListener("resize", close);
    };
  }, [onClose]);

  const width = 200;
  const height = items.length * 32 + 12;
  const left = Math.min(x, window.innerWidth - width - 8);
  const top = Math.min(y, window.innerHeight - height - 8);

  return (
    <div
      ref={ref}
      role="menu"
      style={{ left, top, width }}
      className="menu-pop elevated fixed z-[70] overflow-hidden p-1"
    >
      {items.map((item) => (
        <div key={item.label}>
          {item.separatorBefore && <div className="my-1 h-px bg-line" />}
          <button
            type="button"
            role="menuitem"
            onClick={() => {
              onClose();
              item.onSelect();
            }}
            className="menu-row w-full text-left"
          >
            {item.label}
          </button>
        </div>
      ))}
    </div>
  );
}
