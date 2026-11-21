const SAFE_DATA_IMAGE = /^data:image\/(png|jpe?g|gif|webp|avif|bmp);base64,[A-Za-z0-9+/=\s]+$/;

export function safeDataImage(value: unknown): string | undefined {
  return typeof value === 'string' && SAFE_DATA_IMAGE.test(value) ? value : undefined;
}
