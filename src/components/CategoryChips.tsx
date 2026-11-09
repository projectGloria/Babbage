import React, { useLayoutEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Clock, Code, File, FileText, Film, Folder, Image, Layers, type LucideIcon } from "lucide-react";
import { CATEGORY_CHIPS, CategoryChip, categoryCounts } from "../lib/categories";
import { Facets, SearchResult } from "../types";
import { formatComma } from "../lib/format";

/* Category segments in one floating capsule. Click toggles include;
   Alt-click toggles exclude. OR within the row, AND with the query.
   Empty categories step back instead of disappearing, so nothing reflows.

   Each segment is icon + label. When the row is too narrow for every label,
   all of them drop to icon only (the count stays), rather than scrolling. */

const ICONS: Record<CategoryChip["id"], LucideIcon> = {
  all: Layers,
  file: File,
  folder: Folder,
  image: Image,
  doc: FileText,
  code: Code,
  media: Film,
  archive: File,
  recent: Clock,
};

interface CategoryChipsProps {
  results: SearchResult[];
  facets?: Facets;
  selected: Set<string>;
  excluded: Set<string>;
  onToggle: (id: string, alt: boolean) => void;
}

export const CategoryChips: React.FC<CategoryChipsProps> = ({ results, facets, selected, excluded, onToggle }) => {
  const { t } = useTranslation("search");
  const counts = facets ? { all: facets.all, recent: facets.recent, ...facets.types } : categoryCounts(results);
  const empty = selected.size === 0 && excluded.size === 0;
  const hasAny = results.length > 0;

  /* Whether the full row (icons, labels, counts) fits. The full row is always
     rendered once, invisibly, as the thing to measure: measuring the visible
     row would not work, because once it is icon-only its own width no longer
     says how wide the labels are. */
  const outerRef = useRef<HTMLDivElement>(null);
  const fullRef = useRef<HTMLDivElement>(null);
  const [compact, setCompact] = useState(false);
  useLayoutEffect(() => {
    const outer = outerRef.current;
    const full = fullRef.current;
    if (!outer || !full) return;
    const measure = () => setCompact(full.offsetWidth > outer.clientWidth);
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(outer);
    observer.observe(full);
    return () => observer.disconnect();
  }, []);

  const body = (chip: CategoryChip, withLabel: boolean) => {
    const Icon = ICONS[chip.id];
    const count = counts[chip.id];
    return (
      <>
        <Icon className="h-3.5 w-3.5 shrink-0" strokeWidth={1.75} />
        {withLabel && <span>{t(`categories.${chip.id}`)}</span>}
        {hasAny && count != null && count > 0 && (
          <span className="t-meta text-[10px] text-subtle">{formatComma(count)}</span>
        )}
      </>
    );
  };

  return (
    <div ref={outerRef} className="min-w-0 flex-1">
      <div aria-hidden="true" className="invisible h-0 overflow-hidden">
        <div ref={fullRef} className="inline-flex w-max items-center gap-0.5 p-1">
          {CATEGORY_CHIPS.map((chip) => (
            <span key={chip.id} className="seg !px-3">{body(chip, true)}</span>
          ))}
        </div>
      </div>
      <div className="control no-scrollbar inline-flex max-w-full items-center gap-0.5 overflow-x-auto rounded-control p-1">
        {CATEGORY_CHIPS.map((chip) => {
          const isExcluded = excluded.has(chip.id);
          const isSelected = chip.id === "all" ? empty : selected.has(chip.id);
          const count = counts[chip.id];
          const dim = hasAny && chip.id !== "all" && !count && !isSelected;
          const label = t(`categories.${chip.id}`);
          const hint = isExcluded ? t("chips.excluded") : t("chips.includeHint");
          return (
            <button
              key={chip.id}
              type="button"
              onClick={(e) => onToggle(chip.id, e.altKey)}
              title={compact ? `${label} · ${hint}` : hint}
              aria-label={label}
              data-active={isSelected && !isExcluded}
              className={`seg ${compact ? "!px-2.5" : "!px-3"} ${isExcluded ? "text-danger line-through" : ""} ${dim ? "opacity-40" : ""}`}
            >
              {body(chip, !compact)}
            </button>
          );
        })}
      </div>
    </div>
  );
};
