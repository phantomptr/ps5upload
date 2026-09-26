#ifndef PS5UPLOAD2_DRIVE_SENSORS_H
#define PS5UPLOAD2_DRIVE_SENSORS_H

#include <stddef.h>

/*
 * Drive SMART / temperature sensors via SCSI LOG SENSE.
 *
 * Enumerates /dev/da0..da9, reads each drive's temperature through
 * CAM pass-through (LOG SENSE page 0x0D), and collects capacity,
 * ident string, and filesystem usage. Also returns fixed-storage
 * summaries (internal SSD + M.2 expansion) via statvfs.
 *
 * Output is a JSON string matching the DriveSensorList shape the
 * engine expects:
 *   {"drives":[{device,sizeBytes,ident,tempC,...},...],
 *    "storage":[{label,fsTotalBytes,...},...]}
 *
 * Returns 0 on success, -1 on failure (with err_reason_out set).
 */
int drive_sensors_get_json(char *out, size_t out_cap, size_t *out_written,
                           const char **err_reason_out);

#endif /* PS5UPLOAD2_DRIVE_SENSORS_H */
