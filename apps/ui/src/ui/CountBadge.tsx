// The amber count on the rail's Needs you item and in the title-bar pill. It is hidden at zero.
export function CountBadge(props: { count: number; title?: string }) {
  if (props.count <= 0) return null;
  return (
    <span className="ui-count" title={props.title}>
      {props.count}
    </span>
  );
}
