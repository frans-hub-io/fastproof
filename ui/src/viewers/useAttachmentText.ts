import { useQuery } from '@tanstack/react-query';
import { fetchAttachmentText } from '../api/client';

export const MAX_TEXT_BYTES = 2 * 1024 * 1024;

export function useAttachmentText(project: string, id: string, maxBytes = MAX_TEXT_BYTES) {
  return useQuery({
    queryKey: ['attachment', project, id, maxBytes],
    queryFn: () => fetchAttachmentText(project, id, maxBytes),
    staleTime: Infinity,
  });
}

export function useAttachmentJson(project: string, id: string) {
  const q = useAttachmentText(project, id);
  let parsed: { ok: true; value: unknown } | { ok: false; error: string } | null = null;
  if (q.data) {
    if (q.data.truncated) parsed = { ok: false, error: 'The attachment is larger than 2 MB; download it to read it.' };
    else {
      try {
        parsed = { ok: true, value: JSON.parse(q.data.text) as unknown };
      } catch (e) {
        parsed = { ok: false, error: `Not valid JSON: ${e instanceof Error ? e.message : String(e)}` };
      }
    }
  }
  return { query: q, parsed };
}
