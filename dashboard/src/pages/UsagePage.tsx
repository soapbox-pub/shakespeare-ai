import { useSeoMeta } from '@unhead/react';
import { BarChart3 } from 'lucide-react';

import { EmptyState } from '@/components/EmptyState';
import { PageHeader } from '@/components/PageHeader';
import { RequireLogin } from '@/components/RequireLogin';
import { Button } from '@/components/ui/button';
import { Skeleton } from '@/components/ui/skeleton';
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table';
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip';
import { useGenerations } from '@/hooks/useCredits';
import { useKeys } from '@/hooks/useKeys';
import { dateTime, tokens, usd } from '@/lib/format';

export default function UsagePage() {
  useSeoMeta({ title: 'Usage · Shakespeare AI' });
  return (
    <RequireLogin what="your usage">
      <Usage />
    </RequireLogin>
  );
}

function Usage() {
  const generations = useGenerations();
  const rows = generations.data?.pages.flatMap((page) => page.data) ?? [];
  const keys = useKeys();
  const keyNames = new Map(keys.data?.pages.flatMap((page) => page.data).map((key) => [key.id, key.name || key.label]));

  return (
    <div className="space-y-6">
      <PageHeader title="Usage" description="Every request, newest first, and what it cost." />
      {generations.isPending ? (
        <Skeleton className="h-64 rounded-xl" />
      ) : generations.isError ? (
        <p className="text-sm text-destructive">{generations.error.message}</p>
      ) : rows.length === 0 ? (
        <EmptyState icon={BarChart3}>No requests yet. Point any OpenAI-compatible client at this server to get started.</EmptyState>
      ) : (
        <div className="overflow-hidden rounded-xl border bg-card shadow-xs">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Date</TableHead>
                <TableHead>Model</TableHead>
                <TableHead className="hidden md:table-cell">Via</TableHead>
                <TableHead className="hidden text-right sm:table-cell">Input</TableHead>
                <TableHead className="hidden text-right sm:table-cell">Output</TableHead>
                <TableHead className="text-right">Cost</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {rows.map((generation) => (
                <TableRow key={generation.id}>
                  <TableCell className="whitespace-nowrap">{dateTime(generation.created)}</TableCell>
                  <TableCell>
                    <code className="rounded-md bg-muted px-1.5 py-0.5 text-xs">{generation.model}</code>
                  </TableCell>
                  <TableCell className="hidden text-muted-foreground md:table-cell">
                    {generation.api_key_id ? (keyNames.get(generation.api_key_id) ?? 'An API key') : 'Nostr'}
                  </TableCell>
                  <TableCell className="hidden text-right tabular-nums sm:table-cell">{tokens(generation.usage.prompt_tokens)}</TableCell>
                  <TableCell className="hidden text-right tabular-nums sm:table-cell">{tokens(generation.usage.completion_tokens)}</TableCell>
                  <TableCell className="text-right tabular-nums">
                    {generation.estimated ? (
                      <Tooltip>
                        <TooltipTrigger className="underline decoration-dotted underline-offset-4">≈ {usd(generation.cost)}</TooltipTrigger>
                        <TooltipContent>The client hung up before the provider counted the tokens, so they were estimated.</TooltipContent>
                      </Tooltip>
                    ) : (
                      usd(generation.cost)
                    )}
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      )}
      {generations.hasNextPage && (
        <Button variant="outline" onClick={() => generations.fetchNextPage()} disabled={generations.isFetchingNextPage}>
          Show more
        </Button>
      )}
    </div>
  );
}
