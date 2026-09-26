#ifndef PS5UPLOAD_INSTALLER_ESCALATE_H
#define PS5UPLOAD_INSTALLER_ESCALATE_H

/* Full self-escalation of the current process to SYSTEM. Returns 0 if every
 * credential write succeeded, -1 if any failed (installs are still attempted;
 * hello reports escalated=false). Requires kstuff's kernel patches to be in
 * place (the boot wait guarantees this on a fresh boot). */
int inst_escalate_self(void);

#endif /* PS5UPLOAD_INSTALLER_ESCALATE_H */
