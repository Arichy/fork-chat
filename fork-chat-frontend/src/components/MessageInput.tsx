import { useQuery } from '@tanstack/react-query';
import { useMemo, useState } from 'react';
import { api } from '../api';
import type { Protocol, PublicProvider, Turn } from '../api/types';
import { Button } from './ui/button';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from './ui/select';
import { Textarea } from './ui/textarea';

interface MessageInputProps {
  parentTurn: Turn | null;
  /** Protocol the current session is locked to. Only providers supporting
   * this protocol (and their models) are shown in the picker. */
  protocol: Protocol;
  onSend: (
    text: string,
    provider: string,
    model: string,
    parentId: string | null,
  ) => void;
  disabled?: boolean;
}

/** A flat `(provider, model)` pair presented in the picker. Encoded in the
 * Select's value as `${provider}|${model}` to keep the API simple. */
type ProviderModelOption = {
  key: string;
  provider: string;
  modelId: string;
  modelName: string;
};

function flatten(
  providers: PublicProvider[],
  protocol: Protocol,
): ProviderModelOption[] {
  const out: ProviderModelOption[] = [];
  for (const p of providers) {
    if (!p.supported_protocols.includes(protocol)) continue;
    for (const m of p.models) {
      out.push({
        key: `${p.name}|${m.id}`,
        provider: p.name,
        modelId: m.id,
        modelName: m.name ?? m.id,
      });
    }
  }
  return out;
}

export function MessageInput({
  parentTurn,
  protocol,
  onSend,
  disabled,
}: MessageInputProps) {
  const [text, setText] = useState('');
  const [selectedKey, setSelectedKey] = useState<string>('');

  const { data: config } = useQuery({
    queryKey: ['config'],
    queryFn: api.config.get,
  });

  const options = useMemo(
    () => flatten(config?.providers ?? [], protocol),
    [config, protocol],
  );

  const parentDefaultKey = useMemo(() => {
    if (!parentTurn?.provider || !parentTurn?.model) return '';
    const key = `${parentTurn.provider}|${parentTurn.model}`;
    return options.some((opt) => opt.key === key) ? key : '';
  }, [options, parentTurn?.provider, parentTurn?.model]);

  const defaultKey = options[0]?.key ?? '';
  const currentKey = selectedKey || parentDefaultKey || defaultKey;
  const current = options.find((o) => o.key === currentKey);

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    if (text.trim() && !disabled && current) {
      onSend(
        text.trim(),
        current.provider,
        current.modelId,
        parentTurn?.id ?? null,
      );
      setText('');
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      handleSubmit(e);
    }
  };

  return (
    <form onSubmit={handleSubmit} className="space-y-2">
      {parentTurn && (
        <div className="text-xs text-muted-foreground">
          Replying to: {parentTurn.user_text?.slice(0, 50)}...
        </div>
      )}
      <Textarea
        value={text}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={handleKeyDown}
        placeholder="Type your message..."
        className="rounded-3xl bg-zinc-100/70 px-4 py-3.5 text-[15px] text-zinc-800 placeholder:text-zinc-500 focus-visible:border-zinc-400 focus-visible:ring-2 focus-visible:ring-zinc-300/60"
        disabled={disabled}
      />
      <div className="flex items-center gap-2">
        <Select
          value={currentKey}
          onValueChange={(v) => setSelectedKey(v ?? '')}
          disabled={disabled || options.length === 0}
        >
          <SelectTrigger className="h-10 flex-1 rounded-2xl border border-zinc-300 bg-white px-3.5 shadow-sm transition-colors hover:border-zinc-400 data-[popup-open]:border-zinc-400 data-[popup-open]:bg-zinc-50 focus-visible:border-zinc-500 focus-visible:ring-2 focus-visible:ring-zinc-300/70">
            <SelectValue
              className="sr-only"
              placeholder={
                options.length === 0
                  ? `No providers configured for protocol "${protocol}"`
                  : 'Select provider / model'
              }
            />
            {current ? (
              <span
                aria-hidden
                className="flex min-w-0 flex-1 items-center gap-2 text-left"
              >
                <span className="rounded-md border border-zinc-200 bg-zinc-50 px-1.5 py-0.5 text-[10px] font-semibold leading-none tracking-wide text-zinc-500 uppercase">
                  {current.provider}
                </span>
                <span className="truncate text-sm font-semibold text-zinc-800">
                  {current.modelName}
                </span>
              </span>
            ) : (
              <span aria-hidden className="truncate text-sm text-zinc-500">
                {options.length === 0
                  ? `No providers configured for protocol "${protocol}"`
                  : 'Select provider / model'}
              </span>
            )}
          </SelectTrigger>
          <SelectContent
            sideOffset={8}
            align="start"
            alignItemWithTrigger={false}
            className="w-[--anchor-width] min-w-[--anchor-width] rounded-2xl border border-zinc-200 bg-white p-1 shadow-xl shadow-zinc-900/10"
          >
            {options.map((opt) => (
              <SelectItem
                key={opt.key}
                value={opt.key}
                className="rounded-xl px-3 py-2.5 data-[highlighted]:bg-zinc-100 data-[highlighted]:!text-zinc-900 data-[selected]:bg-zinc-100 data-[selected]:!text-zinc-900 focus:bg-zinc-100 focus:!text-zinc-900"
              >
                <span className="flex min-w-0 flex-col">
                  <span className="truncate pr-5 text-sm font-semibold leading-tight text-zinc-900">
                    {opt.modelName}
                  </span>
                  <span className="mt-0.5 truncate pr-5 text-[11px] font-medium text-zinc-500">
                    {opt.provider}
                  </span>
                </span>
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Button
          type="submit"
          disabled={disabled || !text.trim() || !current}
          className="h-10 rounded-2xl px-5 font-semibold"
        >
          Send
        </Button>
      </div>
    </form>
  );
}
