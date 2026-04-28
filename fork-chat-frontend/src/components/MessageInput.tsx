import { useQuery } from '@tanstack/react-query';
import { useState } from 'react';
import { api } from '../api';
import type { Model, Turn } from '../api/types';
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
  onSend: (text: string, model: string, parentId: string | null) => void;
  disabled?: boolean;
}

export function MessageInput({
  parentTurn,
  onSend,
  disabled,
}: MessageInputProps) {
  const [text, setText] = useState('');
  const [selectedModel, setSelectedModel] = useState('');

  const { data: config } = useQuery({
    queryKey: ['config'],
    queryFn: api.config.get,
  });

  const models: Model[] = config?.models ?? [];
  const defaultModel = models[0]?.id ?? '';
  const currentModel = selectedModel || defaultModel;

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    if (text.trim() && !disabled && currentModel) {
      onSend(text.trim(), currentModel, parentTurn?.id ?? null);
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
          value={currentModel}
          onValueChange={(v) => setSelectedModel(v ?? '')}
          disabled={disabled}
        >
          <SelectTrigger className="flex-1">
            <SelectValue placeholder="Select model" />
          </SelectTrigger>
          <SelectContent>
            {models.map((model) => (
              <SelectItem key={model.id} value={model.id}>
                {model.name}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Button type="submit" disabled={disabled || !text.trim()}>
          Send
        </Button>
      </div>
    </form>
  );
}
