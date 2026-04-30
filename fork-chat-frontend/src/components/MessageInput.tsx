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
  label: string;
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
        label: `${p.name} · ${m.name ?? m.id}`,
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
        disabled={disabled}
      />
      <div className="flex gap-2">
        <Select
          value={currentKey}
          onValueChange={(v) => setSelectedKey(v ?? '')}
          disabled={disabled || options.length === 0}
        >
          <SelectTrigger className="flex-1">
            <SelectValue
              placeholder={
                options.length === 0
                  ? `No providers configured for protocol "${protocol}"`
                  : 'Select provider / model'
              }
            />
          </SelectTrigger>
          <SelectContent>
            {options.map((opt) => (
              <SelectItem key={opt.key} value={opt.key}>
                {opt.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Button type="submit" disabled={disabled || !text.trim() || !current}>
          Send
        </Button>
      </div>
    </form>
  );
}
