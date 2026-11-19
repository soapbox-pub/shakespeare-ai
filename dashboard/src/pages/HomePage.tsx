import { useSeoMeta } from '@unhead/react';
import { ArrowRight } from 'lucide-react';
import { Link } from 'react-router-dom';

import { LoginArea } from '@/components/auth/LoginArea';
import { CopyButton } from '@/components/CopyButton';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Skeleton } from '@/components/ui/skeleton';
import { useCurrentUser } from '@/hooks/useCurrentUser';
import { useModels } from '@/hooks/useCredits';
import type { Model } from '@/lib/api';
import { perMillion, price } from '@/lib/format';

export default function HomePage() {
  useSeoMeta({
    title: 'Shakespeare AI',
    description: 'An OpenAI-compatible AI API. Sign in with Nostr, and pay as you go by card, Lightning or bitcoin.',
  });

  const { user } = useCurrentUser();
  const models = useModels();
  const endpoint = `${location.origin}/v1`;

  return (
    <div className="space-y-16">
      <section className="space-y-6 pt-4 text-center sm:pt-8">
        <h1 className="text-4xl font-semibold tracking-tight sm:text-5xl">One API for the best AI models</h1>
        <p className="mx-auto max-w-2xl text-lg text-muted-foreground">
          OpenAI-compatible, so any client works. Sign in with your Nostr key instead of an API key, and pay only
          for what you use, by card, Lightning or bitcoin.
        </p>
        <div className="mx-auto flex max-w-md items-center gap-2 rounded-lg border bg-muted/40 py-1 pr-1 pl-4">
          <code className="flex-1 truncate text-left text-sm">{endpoint}</code>
          <CopyButton value={endpoint} label="Copy the API endpoint" />
        </div>
        <div className="flex justify-center">
          {user ? (
            <Button asChild>
              <Link to="/credits">
                Your credits <ArrowRight className="size-4" />
              </Link>
            </Button>
          ) : (
            <LoginArea />
          )}
        </div>
      </section>

      <section className="space-y-6">
        <h2 className="text-xl font-semibold tracking-tight">Models</h2>
        {models.isError && <p className="text-muted-foreground">Couldn't load the models. Refresh to try again.</p>}
        <div className="grid gap-4 sm:grid-cols-2">
          {models.isPending && Array.from({ length: 4 }, (_, i) => <Skeleton key={i} className="h-40 rounded-xl" />)}
          {models.data?.map((model) => <ModelCard key={model.id} model={model} />)}
        </div>
      </section>
    </div>
  );
}

function ModelCard({ model }: { model: Model }) {
  return (
    <Card className="gap-4">
      <CardHeader>
        <div className="flex items-start justify-between gap-2">
          <CardTitle>{model.name}</CardTitle>
          <Badge variant="secondary">{model.type === 'image' ? 'Images' : 'Chat'}</Badge>
        </div>
        <div className="flex items-center gap-1 text-muted-foreground">
          <code className="text-xs">{model.id}</code>
          <CopyButton value={model.id} label={`Copy ${model.id}`} />
        </div>
        {model.description && <CardDescription>{model.description}</CardDescription>}
      </CardHeader>
      <CardContent className="mt-auto text-sm">
        {model.type === 'chat' ? (
          <dl className="grid grid-cols-3 gap-2">
            <Stat label="Input" value={`${perMillion(model.pricing.prompt)}/M`} />
            <Stat label="Output" value={`${perMillion(model.pricing.completion)}/M`} />
            {model.context_window && <Stat label="Context" value={`${Math.round(model.context_window / 1000)}K`} />}
          </dl>
        ) : (
          <dl className="grid grid-cols-3 gap-2">
            <Stat label="Low" value={price(Number(model.pricing.low_1024))} />
            <Stat label="Medium" value={price(Number(model.pricing.medium_1024))} />
            <Stat label="High" value={price(Number(model.pricing.high_1024))} />
          </dl>
        )}
      </CardContent>
    </Card>
  );
}

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <dt className="text-xs text-muted-foreground">{label}</dt>
      <dd className="font-medium tabular-nums">{value}</dd>
    </div>
  );
}
