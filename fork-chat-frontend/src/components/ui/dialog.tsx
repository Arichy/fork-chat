import { Dialog } from '@base-ui/react/dialog';
import { cn } from '@/lib/utils';

function DialogRoot({
  open,
  onOpenChange,
  children,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  children: React.ReactNode;
}) {
  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      {children}
    </Dialog.Root>
  );
}

function DialogContent({ className, children, ...props }: Dialog.Popup.Props) {
  return (
    <Dialog.Portal>
      <Dialog.Backdrop className="fixed inset-0 bg-black/50 opacity-0 transition-all duration-300 data-[open]:opacity-100 data-[starting-style]:opacity-0! data-[ending-style]:opacity-0!" />
      <Dialog.Viewport className="fixed inset-0 flex items-center justify-center">
        <Dialog.Popup
          data-dialog-popup=""
          className={cn(
            'relative bg-background rounded-xl border shadow-lg p-6 w-full max-w-2xl max-h-[85vh] overflow-auto overscroll-none',
            'opacity-0 scale-95 transition-all duration-300',
            'data-[open]:opacity-100 data-[open]:scale-100',
            'data-[starting-style]:opacity-0! data-[starting-style]:scale-95!',
            'data-[ending-style]:opacity-0! data-[ending-style]:scale-95!',
            className,
          )}
          {...props}
        >
          {children}
        </Dialog.Popup>
      </Dialog.Viewport>
    </Dialog.Portal>
  );
}

function DialogHeader({
  className,
  ...props
}: React.HTMLAttributes<HTMLDivElement>) {
  return (
    <div className={cn('flex flex-col gap-1 mb-4', className)} {...props} />
  );
}

function DialogFooter({
  className,
  ...props
}: React.HTMLAttributes<HTMLDivElement>) {
  return (
    <div className={cn('flex justify-end gap-2 mt-4', className)} {...props} />
  );
}

function DialogTitle({ className, ...props }: Dialog.Title.Props) {
  return (
    <Dialog.Title
      className={cn('text-lg font-semibold', className)}
      {...props}
    />
  );
}

function DialogDescription({ className, ...props }: Dialog.Description.Props) {
  return (
    <Dialog.Description
      className={cn('text-sm text-muted-foreground', className)}
      {...props}
    />
  );
}

function DialogClose({ className, children, ...props }: Dialog.Close.Props) {
  return (
    <Dialog.Close
      className={cn(
        'absolute top-4 right-4 p-1 rounded-sm opacity-70 hover:opacity-100 cursor-pointer',
        className,
      )}
      {...props}
    >
      {children ?? '✕'}
    </Dialog.Close>
  );
}

export {
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogRoot as Dialog,
  DialogTitle,
};
