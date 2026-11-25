export function StateChip(props: { projectKey: string; caseNumber: number }) {
  return <span hidden data-project={props.projectKey} />;
}
