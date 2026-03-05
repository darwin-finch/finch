import { ClientSDK, RequestOptions } from "../lib/sdks.js";
import { CustomField } from "../models/components/customfield.js";
import { CustomFieldCreate } from "../models/components/customfieldcreate.js";
import { CustomFieldsDeleteRequest } from "../models/operations/customfieldsdelete.js";
import { CustomFieldsGetRequest } from "../models/operations/customfieldsget.js";
import { CustomFieldsListRequest, CustomFieldsListResponse } from "../models/operations/customfieldslist.js";
import { CustomFieldsUpdateRequest } from "../models/operations/customfieldsupdate.js";
import { PageIterator } from "../types/operations.js";
export declare class CustomFields extends ClientSDK {
    /**
     * List Custom Fields
     *
     * @remarks
     * List custom fields.
     *
     * **Scopes**: `custom_fields:read` `custom_fields:write`
     */
    list(request: CustomFieldsListRequest, options?: RequestOptions): Promise<PageIterator<CustomFieldsListResponse, {
        page: number;
    }>>;
    /**
     * Create Custom Field
     *
     * @remarks
     * Create a custom field.
     *
     * **Scopes**: `custom_fields:write`
     */
    create(request: CustomFieldCreate, options?: RequestOptions): Promise<CustomField>;
    /**
     * Get Custom Field
     *
     * @remarks
     * Get a custom field by ID.
     *
     * **Scopes**: `custom_fields:read` `custom_fields:write`
     */
    get(request: CustomFieldsGetRequest, options?: RequestOptions): Promise<CustomField>;
    /**
     * Update Custom Field
     *
     * @remarks
     * Update a custom field.
     *
     * **Scopes**: `custom_fields:write`
     */
    update(request: CustomFieldsUpdateRequest, options?: RequestOptions): Promise<CustomField>;
    /**
     * Delete Custom Field
     *
     * @remarks
     * Delete a custom field.
     *
     * **Scopes**: `custom_fields:write`
     */
    delete(request: CustomFieldsDeleteRequest, options?: RequestOptions): Promise<void>;
}
//# sourceMappingURL=customfields.d.ts.map