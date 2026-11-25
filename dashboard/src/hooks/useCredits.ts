import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useMemo } from 'react';

import { useCurrentUser } from '@/hooks/useCurrentUser';
import { ApiClient, type Credits, isUnsettled, type List, type Model, type Payment, type PaymentMethod, type PaymentMethods, publicRequest } from '@/lib/api';

/**
 * An API client signed in as the current account, or undefined when logged
 * out. Recreated when the account changes, so switching accounts never signs
 * with the last one's key.
 */
export function useApi(): ApiClient | undefined {
  const { user } = useCurrentUser();
  return useMemo(() => (user ? new ApiClient(user.signer) : undefined), [user]);
}

/** Query keys start with the account, so one account's data never shows for another. */
export function useAccountKey(): string | undefined {
  return useCurrentUser().user?.pubkey;
}

/**
 * The balance. While credit is pending it's checked every half minute, and
 * the payment history refreshed when what's pending changes, so a bitcoin
 * payment that confirms shows without a reload.
 */
export function useBalance() {
  const api = useApi();
  const pubkey = useAccountKey();
  const queryClient = useQueryClient();
  return useQuery({
    queryKey: ['balance', pubkey],
    queryFn: async () => {
      const last = queryClient.getQueryData<Credits>(['balance', pubkey]);
      const credits = await api!.balance();
      if (last && last.pending !== credits.pending) {
        void queryClient.invalidateQueries({ queryKey: ['payments', pubkey] });
      }
      return credits;
    },
    enabled: !!api,
    staleTime: 10_000,
    refetchInterval: (query) => (query.state.data?.pending ? 30_000 : false),
  });
}

export function usePaymentMethods() {
  return useQuery({
    queryKey: ['payment-methods'],
    queryFn: () => publicRequest<PaymentMethods>('/v1/credits/methods'),
  });
}

export function useModels() {
  return useQuery({
    queryKey: ['models'],
    queryFn: async () => (await publicRequest<{ data: Model[] }>('/v1/models')).data,
  });
}

/** A paged list, newest first, loading 50 more at a time. */
export function useList<T extends { id: string }>(name: string, fetch: (api: ApiClient, after?: string) => Promise<List<T>>) {
  const api = useApi();
  const pubkey = useAccountKey();
  return useInfiniteQuery({
    queryKey: [name, pubkey],
    queryFn: ({ pageParam }) => fetch(api!, pageParam),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => (last.has_more ? last.data.at(-1)?.id : undefined),
    enabled: !!api,
    staleTime: 10_000,
  });
}

export function usePayments() {
  return useList('payments', (api, after) => api.payments(after));
}

export function useGenerations() {
  return useList('generations', (api, after) => api.generations(after));
}

/** Watch a payment until it's settled, then refresh the balance and history. */
export function usePayment(id: string | undefined, method: PaymentMethod | undefined) {
  const api = useApi();
  const pubkey = useAccountKey();
  const queryClient = useQueryClient();
  return useQuery({
    queryKey: ['payment', pubkey, id],
    queryFn: async () => {
      const last = queryClient.getQueryData<Payment>(['payment', pubkey, id]);
      const payment = await api!.payment(id!);
      // Paid, expired, or sent bitcoin or monero: the balance and history show it.
      const received = (p: Payment) => p.bitcoin?.received ?? p.monero?.received;
      if (!last || payment.status !== last.status || received(payment) !== received(last)) {
        void queryClient.invalidateQueries({ queryKey: ['balance', pubkey] });
        void queryClient.invalidateQueries({ queryKey: ['payments', pubkey] });
      }
      return payment;
    },
    enabled: !!api && !!id,
    // Asking about a Lightning, bitcoin or monero payment checks on it there
    // and then (an address at most every 10 seconds), so it's worth asking
    // often. Stripe's is completed by its webhook.
    refetchInterval: (query) => {
      const payment = query.state.data;
      if (payment && !isUnsettled(payment)) return false;
      return method === 'lightning' ? 3_000 : method === 'bitcoin' || method === 'monero' ? 10_000 : 15_000;
    },
    staleTime: 0,
  });
}

export function useAddCredits() {
  const api = useApi();
  const pubkey = useAccountKey();
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (input: { amount: number; method: PaymentMethod; redirect_url?: string }) => api!.addCredits(input),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['payments', pubkey] }),
  });
}

export function useGiftcards() {
  return useList('giftcards', (api, after) => api.giftcards(after));
}

export function useCreateGiftcards() {
  const api = useApi();
  const pubkey = useAccountKey();
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (input: { amount: number; quantity: number }) => (await api!.createGiftcards(input)).data,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['balance', pubkey] });
      void queryClient.invalidateQueries({ queryKey: ['giftcards', pubkey] });
    },
  });
}

export function useCancelGiftcard() {
  const api = useApi();
  const pubkey = useAccountKey();
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => api!.cancelGiftcard(id),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['balance', pubkey] });
      void queryClient.invalidateQueries({ queryKey: ['giftcards', pubkey] });
    },
  });
}

export function useRedeemGiftcard() {
  const api = useApi();
  const pubkey = useAccountKey();
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (code: string) => api!.redeemGiftcard(code),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['balance', pubkey] }),
  });
}
