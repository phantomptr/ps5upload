/** Limits of the payload's snapshot format (payload/src/backup.c), shown on the
 *  screen so they are known before a restore, not discovered by one. */

/** BACKUPS_KEEP_PER_TAG: older snapshots of the same name are deleted. */
export const SNAPSHOTS_KEPT_PER_NAME = 5;

/** snapshot_tree_inner stops below this many levels under the picked folder. */
export const SNAPSHOT_MAX_DEPTH = 8;
