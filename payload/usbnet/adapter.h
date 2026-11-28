/* SPDX-License-Identifier: GPL-3.0-or-later
 * Adapted from tom-ps5-usbnet (https://github.com/wujiezero/tom-ps5-usbnet, GPL-3.0-or-later),
 * include/adapter.h, for PS5Upload's USB adapter daemon (payload/usbnet/README.md). */
#ifndef ADAPTER_H
#define ADAPTER_H
#include "ncm.h"

#include <dev/usb/usb.h>
#include <dev/usb/usb_ioctl.h>

#include <stddef.h>
#include <stdint.h>

/* How long the bus is watched for the adapter. The helper only launches the daemon once it has
 * seen one, so this covers a node that is being recreated (a replug). */
#ifndef ADAPTER_WAIT_SECONDS
#define ADAPTER_WAIT_SECONDS 5
#endif
#define ADAPTER_NONE 2                  /* adapter_main(): none, or none it can use */

extern int adapter_gone;

struct adapter {
    char path[32];
    int fd;
    int original_index, ecm_index, active_index;
    uint16_t product;
    int unusable;                       /* set by `run`: the adapter is not one this can drive */
    unsigned data_iface;                /* what the end of the run puts back */
    struct ncm_function ecm;
};

/* Finds the adapter, runs `run` with it open, puts it back as it was found. Returns
 * what main() should return, or ADAPTER_NONE if there was no adapter, or `run` set
 * `unusable`. */
int adapter_main(void (*run)(struct adapter *adapter));
/* A device-to-host request. Returns the bytes read, or -1. */
int adapter_control_in(int fd, const uint8_t setup[8], uint8_t *data);
int adapter_read_config(struct adapter *adapter, int index, uint8_t *data, size_t capacity);
int adapter_select_config(struct adapter *adapter, int index);
int adapter_driver_active(struct adapter *adapter);
/* Enables the ECM configuration's data interface, then leaves it. Only if ecm_index is
 * not -1: `ecm` may be an NCM function's, for the address alone. */
int adapter_pass_through_ecm(struct adapter *adapter);
/* The adapter's own address. Never to be logged. */
int adapter_read_mac(struct adapter *adapter, uint8_t mac[6]);
#endif
