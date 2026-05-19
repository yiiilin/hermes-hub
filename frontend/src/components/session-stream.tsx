type SessionStreamProps = {
  text: string;
};

export function SessionStream({ text }: SessionStreamProps) {
  return <pre className="stream">{text}</pre>;
}
