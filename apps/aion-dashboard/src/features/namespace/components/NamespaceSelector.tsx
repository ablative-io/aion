import {
  Button,
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectLabel,
  SelectTrigger,
  SelectValue,
  Skeleton,
} from '@/components/ui';
import { cn } from '@/lib/utils';

import { applyNamespaceSelection, useNamespace } from '../context/NamespaceContext';

type NamespaceSelectorProps = {
  className?: string;
};

export function NamespaceSelector({ className }: NamespaceSelectorProps) {
  const {
    error,
    isError,
    isLoading,
    namespaces,
    refetch,
    selectedNamespace,
    setSelectedNamespace,
  } = useNamespace();

  if (isLoading) {
    return (
      <div className={cn('flex min-w-48 flex-col gap-2', className)}>
        <span className="text-muted-foreground text-xs font-medium">Namespace</span>
        <Skeleton className="h-9 w-48" data-testid="namespace-selector-loading" />
      </div>
    );
  }

  if (isError) {
    return (
      <div
        className={cn(
          'border-destructive/40 flex min-w-48 flex-col gap-2 rounded-md border p-3',
          className
        )}
        role="alert"
      >
        <span className="text-destructive text-sm font-medium">Namespaces unavailable</span>
        <p className="text-muted-foreground text-xs">
          {error?.message ?? 'The namespace list could not be loaded.'}
        </p>
        <Button
          className="w-fit"
          onClick={() => void refetch()}
          size="sm"
          type="button"
          variant="outline"
        >
          Retry
        </Button>
      </div>
    );
  }

  if (namespaces.length === 0) {
    return (
      <div className={cn('flex min-w-48 flex-col gap-1', className)}>
        <span className="text-muted-foreground text-xs font-medium">Namespace</span>
        <div className="rounded-md border px-3 py-2 text-muted-foreground text-sm">
          No namespaces available
        </div>
      </div>
    );
  }

  return (
    <div className={cn('flex min-w-48 flex-col gap-2', className)}>
      <label className="text-muted-foreground text-xs font-medium" htmlFor="namespace-selector">
        Namespace
      </label>
      <Select
        onValueChange={(namespace) =>
          applyNamespaceSelection(setSelectedNamespace, namespace, namespaces)
        }
        value={selectedNamespace ?? undefined}
      >
        <SelectTrigger aria-label="Namespace" className="w-48" id="namespace-selector">
          <SelectValue placeholder="Select namespace" />
        </SelectTrigger>
        <SelectContent>
          <SelectGroup>
            <SelectLabel>Namespaces</SelectLabel>
            {namespaces.map((namespace) => (
              <SelectItem key={namespace} value={namespace}>
                {namespace}
              </SelectItem>
            ))}
          </SelectGroup>
        </SelectContent>
      </Select>
    </div>
  );
}
