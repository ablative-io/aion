import { useQuery } from '@tanstack/react-query';

import { authoringFacade } from '../lib/facade';

export function useAuthoringDocuments() {
  return useQuery({
    queryKey: ['authoring', 'documents'],
    queryFn: () => authoringFacade.listDocuments(),
  });
}

export function useAuthoringDocument(path: string | null) {
  return useQuery({
    queryKey: ['authoring', 'document', path],
    queryFn: () => authoringFacade.loadDocument(path ?? ''),
    enabled: path !== null,
  });
}
