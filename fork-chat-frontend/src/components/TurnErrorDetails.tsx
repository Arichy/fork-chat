type TurnErrorDetailsProps = {
  error: Record<string, unknown>;
};

function getStringField(error: Record<string, unknown>, field: string): string {
  const value = error[field];
  return typeof value === 'string' ? value : '';
}

function getStringArrayField(
  error: Record<string, unknown>,
  field: string,
): string[] {
  const value = error[field];
  if (!Array.isArray(value)) return [];

  // Failed turns created by older backend versions may have arbitrary values in
  // the error JSON, so keep only strings before rendering the diagnostic list.
  return value.filter((entry): entry is string => typeof entry === 'string');
}

/** Renders failed-turn diagnostics without making the main chat view noisy. */
export function TurnErrorDetails({ error }: TurnErrorDetailsProps) {
  const message = getStringField(error, 'message') || JSON.stringify(error);
  const kind = getStringField(error, 'kind');
  const chain = getStringArrayField(error, 'chain');
  const debug = getStringField(error, 'debug');
  const hasDiagnostics = chain.length > 0 || debug.length > 0;

  return (
    <div className="rounded-md border border-destructive/30 bg-destructive/10 p-3 text-sm">
      <div className="flex flex-col gap-1">
        <div className="font-medium text-destructive">
          {kind ? `Error: ${kind}` : 'Error'}
        </div>
        <div className="break-words text-foreground">{message}</div>
      </div>

      {hasDiagnostics && (
        <details className="mt-3">
          <summary className="cursor-pointer text-xs font-medium text-muted-foreground">
            Diagnostics
          </summary>
          <div className="mt-2 flex flex-col gap-3">
            {chain.length > 0 && (
              <div>
                <div className="mb-1 text-xs font-medium text-muted-foreground">
                  Error Chain
                </div>
                <ol className="list-decimal pl-5 text-xs text-foreground">
                  {chain.map((entry, index) => (
                    <li className="break-words" key={`${index}-${entry}`}>
                      {entry}
                    </li>
                  ))}
                </ol>
              </div>
            )}

            {debug && (
              <div>
                <div className="mb-1 text-xs font-medium text-muted-foreground">
                  Debug
                </div>
                <pre className="max-h-64 overflow-auto rounded border bg-background p-2 text-xs text-foreground">
                  {debug}
                </pre>
              </div>
            )}
          </div>
        </details>
      )}
    </div>
  );
}
