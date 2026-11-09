import { DoctorReport } from "../types";

export interface ReportOptions {
  title?: string;
  whatHappened?: string;
  steps?: string;
  report?: DoctorReport | null;
  diagnostics?: string;
}

/**
 * Copies diagnostics to clipboard locally without opening any external links.
 */
export async function openGitHubBugReport(options: ReportOptions = {}): Promise<{ copied: boolean }> {
  let diagnosticsJson = options.diagnostics ?? "";
  if (!diagnosticsJson && options.report) {
    diagnosticsJson = JSON.stringify(options.report, null, 2);
  }

  let copied = false;
  if (diagnosticsJson) {
    try {
      await navigator.clipboard.writeText(diagnosticsJson);
      copied = true;
    } catch (err) {
      console.warn("Could not copy diagnostics to clipboard:", err);
    }
  }

  return { copied };
}
