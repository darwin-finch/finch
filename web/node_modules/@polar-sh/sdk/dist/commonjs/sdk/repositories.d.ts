import { ClientSDK, RequestOptions } from "../lib/sdks.js";
import { Repository } from "../models/components/repository.js";
import { RepositoriesGetRequest } from "../models/operations/repositoriesget.js";
import { RepositoriesListRequest, RepositoriesListResponse } from "../models/operations/repositorieslist.js";
import { RepositoriesUpdateRequest } from "../models/operations/repositoriesupdate.js";
import { PageIterator } from "../types/operations.js";
export declare class Repositories extends ClientSDK {
    /**
     * List Repositories
     *
     * @remarks
     * List repositories.
     *
     * **Scopes**: `repositories:read` `repositories:write`
     */
    list(request: RepositoriesListRequest, options?: RequestOptions): Promise<PageIterator<RepositoriesListResponse, {
        page: number;
    }>>;
    /**
     * Get Repository
     *
     * @remarks
     * Get a repository by ID.
     *
     * **Scopes**: `repositories:read` `repositories:write`
     */
    get(request: RepositoriesGetRequest, options?: RequestOptions): Promise<Repository>;
    /**
     * Update Repository
     *
     * @remarks
     * Update a repository.
     *
     * **Scopes**: `repositories:write`
     */
    update(request: RepositoriesUpdateRequest, options?: RequestOptions): Promise<Repository>;
}
//# sourceMappingURL=repositories.d.ts.map