I have addressed your requests as follows:

1.  **"when stopped that's not say failed."**:
    *   I have modified `desktop/src/hooks/useTransferEvents.ts` to correctly differentiate between a stopped transfer and an error. Now, when a transfer is stopped by the user, the status will explicitly say "Stopped" instead of showing an "Error" message.

2.  **"also stopped can we like do yellow and failed show read."**:
    *   I have added new CSS classes (`.status-stopped` and `.status-error`) to `desktop/src/styles/theme.css`.
    *   I have updated `desktop/src/components/TransferProgress.tsx` to apply these classes:
        *   The "Stopped" status will now be displayed in yellow.
        *   Any "Error: [message]" status will now be displayed in red.

3.  **"let's get rid of Avg."**:
    *   I have modified `desktop/src/App.tsx` to remove the "Avg " prefix from the speed labels (e.g., "Avg Payload", "Avg FTP", "Avg Total") in the main transfer progress display. The history display already did not include "Avg".

**Regarding your request to see "how many files are going to be uploaded by payload and how many files are going to be uploaded by ftp.":**

Upon investigating the application's data structures (`desktop/src/types.ts`, `desktop/src/App.tsx`, `desktop/src/hooks/useTransferEvents.ts`), the transfer progress events (`TransferProgressEvent`) and status snapshots (`TransferStatusSnapshot`) currently only provide a single total count for `files_sent` (or `files`). There are no separate fields available to distinguish between files transferred specifically via "payload" or "FTP".

Therefore, I cannot implement this feature without modifications to the backend system that generates these transfer events to include more granular file count information.

These changes should improve the clarity and visual feedback of the transfer status in the UI.