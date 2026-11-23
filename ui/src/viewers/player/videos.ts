import type { Attachment, AttachmentRef } from '../../api/types';
import { attachmentKind, toAttachment } from '../../lib/attachments';

export function videosOf(attachments: readonly AttachmentRef[]): Attachment[] {
  return attachments.map(toAttachment).filter((a): a is Attachment => a !== null && attachmentKind(a) === 'video');
}
