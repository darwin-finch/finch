import { ClientSDK, RequestOptions } from "../lib/sdks.js";
import { Organization } from "../models/components/organization.js";
import { OrganizationCreate } from "../models/components/organizationcreate.js";
import { OrganizationsGetRequest } from "../models/operations/organizationsget.js";
import { OrganizationsListRequest, OrganizationsListResponse } from "../models/operations/organizationslist.js";
import { OrganizationsUpdateRequest } from "../models/operations/organizationsupdate.js";
import { PageIterator } from "../types/operations.js";
export declare class Organizations extends ClientSDK {
    /**
     * List Organizations
     *
     * @remarks
     * List organizations.
     *
     * **Scopes**: `organizations:read` `organizations:write`
     */
    list(request: OrganizationsListRequest, options?: RequestOptions): Promise<PageIterator<OrganizationsListResponse, {
        page: number;
    }>>;
    /**
     * Create Organization
     *
     * @remarks
     * Create an organization.
     *
     * **Scopes**: `organizations:write`
     */
    create(request: OrganizationCreate, options?: RequestOptions): Promise<Organization>;
    /**
     * Get Organization
     *
     * @remarks
     * Get an organization by ID.
     *
     * **Scopes**: `organizations:read` `organizations:write`
     */
    get(request: OrganizationsGetRequest, options?: RequestOptions): Promise<Organization>;
    /**
     * Update Organization
     *
     * @remarks
     * Update an organization.
     *
     * **Scopes**: `organizations:write`
     */
    update(request: OrganizationsUpdateRequest, options?: RequestOptions): Promise<Organization>;
}
//# sourceMappingURL=organizations.d.ts.map