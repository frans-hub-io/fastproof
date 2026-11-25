export function SharedRefresh(props: { projectKey: string; launchId: string }) {
  return <span hidden data-project={props.projectKey} />;
}
