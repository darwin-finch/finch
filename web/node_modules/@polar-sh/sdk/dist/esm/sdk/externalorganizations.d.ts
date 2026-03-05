import { ClientSDK, RequestOptions } from "../lib/sdks.js";
import { ExternalOrganizationsListRequest, ExternalOrganizationsListResponse } from "../models/operations/externalorganizationslist.js";
import { PageIterator } from "../types/operations.js";
export declare class ExternalOrganizations extends ClientSDK {
    /**
     * List External Organizations
     *
     * @remarks
     * List external organizations.
     *
     * **Scopes**: `external_organizations:read`
     */
    list(request: ExternalOrganizationsListRequest, options?: RequestOptions): Promise<PageIterator<ExternalOrganizationsListResponse, {
        page: number;
    }>>;
}
//# sourceMappingURL=externalorganizations.d.ts.map