import { useSeoMeta } from '@unhead/react';
import { ArrowRight, BookOpen, ImageIcon, KeyRound, MessageSquare, Plug, Wallet } from 'lucide-react';
import { Link } from 'react-router-dom';

import { LoginArea } from '@/components/auth/LoginArea';
import { CopyButton } from '@/components/CopyButton';
import { Logo } from '@/components/Logo';
import { SectionHeader } from '@/components/PageHeader';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Skeleton } from '@/components/ui/skeleton';
import { useCurrentUser } from '@/hooks/useCurrentUser';
import { useModels, usePaymentMethods } from '@/hooks/useCredits';
import type { Model, PaymentMethod } from '@/lib/api';
import { perMillion, price } from '@/lib/format';

const METHOD_NAMES: Record<PaymentMethod, string> = { stripe: 'card', lightning: 'Lightning', bitcoin: 'bitcoin', monero: 'monero' };

/** "card, Lightning, bitcoin or monero", for the methods this server takes. */
function methodList(methods: PaymentMethod[]): string {
  const names = methods.map((m) => METHOD_NAMES[m]).filter(Boolean);
  return names.length > 1 ? `${names.slice(0, -1).join(', ')} or ${names.at(-1)}` : (names[0] ?? '');
}

/**
 * How credits are paid for. It names the ways this server takes once it's
 * asked; before then, as in the prerendered page, it names none, so it's never
 * wrong about a server it hasn't asked.
 */
function usePayText(): string {
  const methods = usePaymentMethods();
  const list = methodList(methods.data?.data.map((m) => m.id) ?? []);
  return list ? `Add credits by ${list} as you need them. No subscription.` : 'Add credits as you need them. No subscription.';
}

export default function HomePage() {
  useSeoMeta({
    title: 'Shakespeare AI',
    description: 'An OpenAI-compatible AI API. Sign in with Nostr, and pay as you go.',
  });

  const { user } = useCurrentUser();
  const models = useModels();
  const endpoint = `${location.origin}/v1`;
  const features = [
    { icon: Plug, title: 'OpenAI-compatible', text: 'Point any client or SDK at the endpoint. Nothing else to change.' },
    { icon: KeyRound, title: 'Your Nostr key', text: 'Sign in and sign requests with it. No email, no password.' },
    { icon: Wallet, title: 'Pay as you go', text: usePayText() },
  ];

  return (
    <div className="space-y-14">
      <section className="relative overflow-hidden rounded-3xl border bg-brand-soft px-5 py-10 text-center sm:px-12 sm:py-16">
        <Logo className="mx-auto mb-6 size-20 drop-shadow-lg sm:size-24" />
        <h1 className="mx-auto max-w-2xl text-4xl font-bold tracking-tight text-balance sm:text-5xl">
          <span className="text-brand">One API</span> for the best AI models
        </h1>
        <p className="mx-auto mt-4 max-w-xl text-lg text-pretty text-muted-foreground">
          OpenAI-compatible, so any client works. Sign in with your Nostr key instead of an API key, and pay only for
          what you use.
        </p>

        <div className="mx-auto mt-8 flex max-w-md items-center gap-2 rounded-xl border bg-background/80 py-1.5 pr-1.5 pl-4 shadow-xs backdrop-blur">
          <span className="text-xs font-medium tracking-wide text-muted-foreground uppercase">Base URL</span>
          <code className="flex-1 truncate text-left text-sm font-medium">{endpoint}</code>
          <CopyButton value={endpoint} label="Copy the API endpoint" />
        </div>

        <div className="mt-6 flex flex-wrap items-center justify-center gap-3">
          {user ? (
            <Button asChild variant="brand" size="lg" className="rounded-full px-6">
              <Link to="/credits">
                Your credits <ArrowRight />
              </Link>
            </Button>
          ) : (
            <LoginArea />
          )}
          <Button asChild variant="outline" size="lg" className="rounded-full px-6">
            <Link to="/docs">
              <BookOpen /> Read the docs
            </Link>
          </Button>
        </div>
      </section>

      <section className="grid gap-3 sm:grid-cols-3">
        {features.map(({ icon: Icon, title, text }) => (
          <div key={title} className="rounded-2xl border bg-card p-5">
            <div className="mb-3 inline-flex rounded-lg bg-accent p-2 text-primary">
              <Icon className="size-4.5" aria-hidden />
            </div>
            <h3 className="font-semibold">{title}</h3>
            <p className="mt-1 text-sm text-muted-foreground">{text}</p>
          </div>
        ))}
      </section>

      <section className="space-y-4">
        <SectionHeader title="Models">
          {models.data && <span className="text-sm text-muted-foreground">{models.data.length} available</span>}
        </SectionHeader>
        {models.isError && <p className="text-muted-foreground">Couldn't load the models. Refresh to try again.</p>}
        <div className="grid gap-4 md:grid-cols-2">
          {models.isPending && Array.from({ length: 4 }, (_, i) => <Skeleton key={i} className="h-48 rounded-2xl" />)}
          {models.data?.map((model) => <ModelCard key={model.id} model={model} />)}
        </div>
      </section>
    </div>
  );
}

function ModelCard({ model }: { model: Model }) {
  const Icon = model.type === 'image' ? ImageIcon : MessageSquare;
  return (
    <article className="group flex flex-col gap-4 rounded-2xl border bg-card p-5 transition-all hover:-translate-y-0.5 hover:border-primary/30 hover:shadow-lg hover:shadow-primary/5">
      <div className="flex items-start gap-3">
        <div className="rounded-xl bg-brand p-2.5 text-white shadow-sm shadow-primary/20">
          <Icon className="size-5" aria-hidden />
        </div>
        <div className="min-w-0 flex-1">
          <div className="flex items-start justify-between gap-2">
            <h3 className="font-semibold leading-snug">{model.name}</h3>
            <Badge variant="secondary" className="shrink-0">{model.type === 'image' ? 'Images' : 'Chat'}</Badge>
          </div>
          <div className="-ml-1 flex items-center gap-0.5 text-muted-foreground">
            <code className="truncate pl-1 text-xs">{model.id}</code>
            <CopyButton value={model.id} label={`Copy ${model.id}`} />
          </div>
        </div>
      </div>
      {model.description && <p className="text-sm text-muted-foreground">{model.description}</p>}
      <dl className="mt-auto grid grid-cols-3 gap-2 text-sm">
        {model.type === 'chat' ? (
          <>
            <Stat label="Input" value={perMillion(model.pricing.prompt)} unit="/M" />
            <Stat label="Output" value={perMillion(model.pricing.completion)} unit="/M" />
            {model.context_window && <Stat label="Context" value={`${Math.round(model.context_window / 1000)}K`} />}
          </>
        ) : (
          <>
            <Stat label="Low" value={price(Number(model.pricing.low_1024))} />
            <Stat label="Medium" value={price(Number(model.pricing.medium_1024))} />
            <Stat label="High" value={price(Number(model.pricing.high_1024))} />
          </>
        )}
      </dl>
    </article>
  );
}

function Stat({ label, value, unit }: { label: string; value: string; unit?: string }) {
  return (
    <div className="rounded-lg bg-muted px-3 py-2">
      <dt className="text-xs text-muted-foreground">{label}</dt>
      <dd className="font-semibold tabular-nums">
        {value}
        {unit && <span className="text-xs font-normal text-muted-foreground">{unit}</span>}
      </dd>
    </div>
  );
}
