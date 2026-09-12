import { PROJECT_TEMPLATES, type CollectionTemplate } from './model'

export function ProjectTemplatePreview({ kind }: { kind: string }) {
  const template = PROJECT_TEMPLATES[kind as keyof typeof PROJECT_TEMPLATES] ?? []
  return <div className="pw-template-preview">
    <strong>{template.length > 0 ? 'Collections created automatically' : 'Starts without collections'}</strong>
    {template.length > 0 ? <TemplateTree items={template} /> : <p>Add collections when you know how this project should be organised.</p>}
  </div>
}

function TemplateTree({ items }: { items: CollectionTemplate[] }) {
  return <ul>{items.map(item => <li key={item.name}><span>{item.name}</span>{item.children && <TemplateTree items={item.children} />}</li>)}</ul>
}
