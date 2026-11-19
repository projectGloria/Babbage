import React from "react";
import { queryTerms, highlightSegments } from "../lib/format";

/** Subtle match highlight inside a filename, per interaction spec.
    No truncation here — the wrapping parent owns ellipsis/clamping. */
export const HighlightedName: React.FC<{ text: string; query: string }> = ({
  text,
  query,
}) => {
  const segs = highlightSegments(text, queryTerms(query));
  return (
    <span>
      {segs.map((s, i) =>
        s.match ? (
          <mark key={i} className="bg-transparent text-ink">
            {s.text}
          </mark>
        ) : (
          <span key={i}>{s.text}</span>
        ),
      )}
    </span>
  );
};
