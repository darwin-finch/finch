import { ClientSDK, RequestOptions } from "../lib/sdks.js";
import { CustomerPortalDownloadablesCustomerPortalDownloadablesGetRequest } from "../models/operations/customerportaldownloadablescustomerportaldownloadablesget.js";
import { CustomerPortalDownloadablesListRequest, CustomerPortalDownloadablesListResponse, CustomerPortalDownloadablesListSecurity } from "../models/operations/customerportaldownloadableslist.js";
import { PageIterator } from "../types/operations.js";
export declare class Downloadables extends ClientSDK {
    /**
     * List Downloadables
     *
     * @remarks
     * **Scopes**: `customer_portal:read` `customer_portal:write`
     */
    list(security: CustomerPortalDownloadablesListSecurity, request: CustomerPortalDownloadablesListRequest, options?: RequestOptions): Promise<PageIterator<CustomerPortalDownloadablesListResponse, {
        page: number;
    }>>;
    /**
     * Get Downloadable
     */
    get(request: CustomerPortalDownloadablesCustomerPortalDownloadablesGetRequest, options?: RequestOptions): Promise<any | undefined>;
}
//# sourceMappingURL=downloadables.d.ts.map