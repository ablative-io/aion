import { Button } from '@/components/ui';
import { ConnectionIndicator } from '@/features/live-feed';

function App() {
  return (
    <main className="min-h-screen bg-background text-foreground">
      <section className="mx-auto flex min-h-screen max-w-5xl flex-col justify-center gap-6 px-6 py-16">
        <div className="flex justify-end">
          <ConnectionIndicator />
        </div>
        <div className="space-y-3">
          <p className="text-sm font-medium text-muted-foreground uppercase tracking-[0.2em]">
            Aion Dashboard
          </p>
          <h1 className="text-4xl font-semibold tracking-tight">Operational UI scaffold</h1>
          <p className="max-w-2xl text-muted-foreground">
            The standalone Vite shell is ready for the workflow list, history timeline, and live
            stream features that ship in later dashboard briefs.
          </p>
        </div>
        <div>
          <Button type="button">Scaffold ready</Button>
        </div>
      </section>
    </main>
  );
}

export { App };
