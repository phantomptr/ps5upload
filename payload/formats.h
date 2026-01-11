/* PS5 Upload - Format Detection */

#ifndef FORMATS_H
#define FORMATS_H

#include <stdbool.h>

#define MAX_FORMATS 10

typedef struct {
    char name[32];        // Format name: "tar", "zip", "rar", etc.
    char extension[16];   // File extension: ".tar", ".zip", etc.
    char command[256];    // Extraction command template
    char binary_path[256]; // Full path to the binary (e.g., "/usr/bin/tar")
    bool available;       // Is this format supported?
} Format;

// Initialize format detection (checks which tools exist on PS5)
void init_formats(void);

// Get list of supported formats
int get_supported_formats(Format *formats, int max_count);

// Get extraction command for a given format
const char* get_extraction_command(const char *format_name, const char *dest_path);

// Send supported formats list as JSON to client
void handle_list_formats(int client_sock);

#endif /* FORMATS_H */
