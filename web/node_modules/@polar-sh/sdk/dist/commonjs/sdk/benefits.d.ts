import { ClientSDK, RequestOptions } from "../lib/sdks.js";
import { Benefit } from "../models/components/benefit.js";
import { BenefitCreate } from "../models/components/benefitcreate.js";
import { BenefitsDeleteRequest } from "../models/operations/benefitsdelete.js";
import { BenefitsGetRequest } from "../models/operations/benefitsget.js";
import { BenefitsGrantsRequest, BenefitsGrantsResponse } from "../models/operations/benefitsgrants.js";
import { BenefitsListRequest, BenefitsListResponse } from "../models/operations/benefitslist.js";
import { BenefitsUpdateRequest } from "../models/operations/benefitsupdate.js";
import { PageIterator } from "../types/operations.js";
export declare class Benefits extends ClientSDK {
    /**
     * List Benefits
     *
     * @remarks
     * List benefits.
     *
     * **Scopes**: `benefits:read` `benefits:write`
     */
    list(request: BenefitsListRequest, options?: RequestOptions): Promise<PageIterator<BenefitsListResponse, {
        page: number;
    }>>;
    /**
     * Create Benefit
     *
     * @remarks
     * Create a benefit.
     *
     * **Scopes**: `benefits:write`
     */
    create(request: BenefitCreate, options?: RequestOptions): Promise<Benefit>;
    /**
     * Get Benefit
     *
     * @remarks
     * Get a benefit by ID.
     *
     * **Scopes**: `benefits:read` `benefits:write`
     */
    get(request: BenefitsGetRequest, options?: RequestOptions): Promise<Benefit>;
    /**
     * Update Benefit
     *
     * @remarks
     * Update a benefit.
     *
     * **Scopes**: `benefits:write`
     */
    update(request: BenefitsUpdateRequest, options?: RequestOptions): Promise<Benefit>;
    /**
     * Delete Benefit
     *
     * @remarks
     * Delete a benefit.
     *
     * > [!WARNING]
     * > Every grants associated with the benefit will be revoked.
     * > Users will lose access to the benefit.
     *
     * **Scopes**: `benefits:write`
     */
    delete(request: BenefitsDeleteRequest, options?: RequestOptions): Promise<void>;
    /**
     * List Benefit Grants
     *
     * @remarks
     * List the individual grants for a benefit.
     *
     * It's especially useful to check if a user has been granted a benefit.
     *
     * **Scopes**: `benefits:read` `benefits:write`
     */
    grants(request: BenefitsGrantsRequest, options?: RequestOptions): Promise<PageIterator<BenefitsGrantsResponse, {
        page: number;
    }>>;
}
//# sourceMappingURL=benefits.d.ts.map