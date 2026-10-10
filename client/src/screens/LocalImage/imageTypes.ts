/** The image files the picker offers: raw disk images, which the OS mounts
 *  itself (exFAT in practice). Mirrors `looks_like_image` in the engine's
 *  local_image.rs. A .ffpkg is not one: no desktop OS can mount it. */
export const LOCAL_IMAGE_EXTENSIONS = ["exfat", "img", "image", "raw"] as const;
