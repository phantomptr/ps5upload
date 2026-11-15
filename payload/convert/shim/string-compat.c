/* The SDK declares stpncpy but its native libc does not export it. */
#include <stddef.h>
#include <string.h>

char *stpncpy(char *destination, const char *source, size_t count) {
    char *cursor = destination;
    while (count && *source) {
        *cursor++ = *source++;
        count--;
    }
    char *end = cursor;
    while (count--) *cursor++ = '\0';
    return end;
}
