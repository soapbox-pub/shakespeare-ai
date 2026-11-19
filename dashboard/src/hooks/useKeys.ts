import { useMutation, useQueryClient } from '@tanstack/react-query';

import { useAccountKey, useApi, useList } from '@/hooks/useCredits';
import type { ApiKeyChanges } from '@/lib/api';

export function useKeys() {
  return useList('keys', (api, after) => api.keys(after));
}

/** Make, change or revoke a key, then refresh the list. */
function useKeyMutation<Input, Output>(run: (api: NonNullable<ReturnType<typeof useApi>>, input: Input) => Promise<Output>) {
  const api = useApi();
  const pubkey = useAccountKey();
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (input: Input) => run(api!, input),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['keys', pubkey] }),
  });
}

export function useCreateKey() {
  return useKeyMutation((api, input: ApiKeyChanges) => api.createKey(input));
}

export function useUpdateKey() {
  return useKeyMutation((api, { id, changes }: { id: string; changes: ApiKeyChanges }) => api.updateKey(id, changes));
}

export function useRevokeKey() {
  return useKeyMutation((api, id: string) => api.revokeKey(id));
}
